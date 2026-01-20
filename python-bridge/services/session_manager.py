"""
Session Manager for Multi-User Support

Tracks extraction jobs by session ID, enabling multiple web clients
to share a single runner instance while keeping their jobs isolated.

Features:
- Session-based job isolation
- Job lifecycle management (create, update, get, delete)
- Job listing and filtering
- Automatic cleanup of old jobs
- Thread-safe operations
"""

import logging
import threading
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from enum import Enum
from typing import Any

logger = logging.getLogger(__name__)


class JobStatus(Enum):
    """Status values for extraction jobs."""

    IDLE = "idle"
    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


class JobMethod(Enum):
    """Extraction method types."""

    VISION = "vision"
    PLAYWRIGHT = "playwright"
    PATTERN = "pattern"
    WEB = "web"
    COMPOSITE = "composite"


@dataclass
class ExtractionJob:
    """Represents an extraction job."""

    job_id: str
    session_id: str
    method: JobMethod
    status: JobStatus = JobStatus.PENDING
    progress: float = 0.0
    progress_message: str = ""
    source: str = ""
    created_at: datetime = field(default_factory=datetime.utcnow)
    started_at: datetime | None = None
    completed_at: datetime | None = None
    error: str | None = None
    result: dict[str, Any] | None = None
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "job_id": self.job_id,
            "session_id": self.session_id,
            "method": self.method.value,
            "status": self.status.value,
            "progress": self.progress,
            "progress_message": self.progress_message,
            "source": self.source,
            "created_at": self.created_at.isoformat() if self.created_at else None,
            "started_at": self.started_at.isoformat() if self.started_at else None,
            "completed_at": self.completed_at.isoformat() if self.completed_at else None,
            "error": self.error,
            "has_results": self.result is not None,
            "metadata": self.metadata,
        }


class SessionManager:
    """
    Manages extraction jobs across multiple client sessions.

    Thread-safe singleton that tracks all jobs and provides
    session-based isolation for multi-user scenarios.
    """

    _instance: "SessionManager | None" = None
    _lock = threading.Lock()
    _initialized: bool = False  # Class-level default to satisfy mypy

    def __new__(cls) -> "SessionManager":
        """Ensure singleton instance."""
        if cls._instance is None:
            with cls._lock:
                if cls._instance is None:
                    cls._instance = super().__new__(cls)
                    cls._instance._initialized = False
        return cls._instance

    def __init__(self) -> None:
        """Initialize the session manager."""
        if self._initialized:
            return

        self._jobs: dict[str, ExtractionJob] = {}
        self._sessions: dict[str, set[str]] = {}  # session_id -> set of job_ids
        self._jobs_lock = threading.RLock()
        self._cleanup_interval = 3600  # 1 hour
        self._job_ttl = 24 * 3600  # 24 hours
        self._initialized = True

        logger.info("SessionManager initialized")

    # =========================================================================
    # Job Lifecycle
    # =========================================================================

    def create_job(
        self,
        job_id: str,
        session_id: str,
        method: str | JobMethod,
        source: str = "",
        metadata: dict[str, Any] | None = None,
    ) -> ExtractionJob:
        """
        Create a new extraction job.

        Args:
            job_id: Unique job identifier
            session_id: Client session identifier
            method: Extraction method (vision, playwright, pattern, web)
            source: Source URL or description
            metadata: Additional job metadata

        Returns:
            The created ExtractionJob
        """
        if isinstance(method, str):
            method = JobMethod(method)

        job = ExtractionJob(
            job_id=job_id,
            session_id=session_id,
            method=method,
            source=source,
            metadata=metadata or {},
        )

        with self._jobs_lock:
            self._jobs[job_id] = job

            # Track job under session
            if session_id not in self._sessions:
                self._sessions[session_id] = set()
            self._sessions[session_id].add(job_id)

        logger.info(f"Created job {job_id} for session {session_id} (method={method.value})")
        return job

    def start_job(self, job_id: str) -> bool:
        """
        Mark a job as started.

        Args:
            job_id: Job identifier

        Returns:
            True if job was found and updated
        """
        with self._jobs_lock:
            job = self._jobs.get(job_id)
            if not job:
                return False

            job.status = JobStatus.RUNNING
            job.started_at = datetime.utcnow()
            return True

    def update_progress(
        self,
        job_id: str,
        progress: float,
        message: str = "",
    ) -> bool:
        """
        Update job progress.

        Args:
            job_id: Job identifier
            progress: Progress percentage (0-100)
            message: Human-readable progress message

        Returns:
            True if job was found and updated
        """
        with self._jobs_lock:
            job = self._jobs.get(job_id)
            if not job:
                return False

            job.progress = min(max(progress, 0), 100)
            job.progress_message = message
            return True

    def complete_job(
        self,
        job_id: str,
        result: dict[str, Any] | None = None,
    ) -> bool:
        """
        Mark a job as completed.

        Args:
            job_id: Job identifier
            result: Job result data

        Returns:
            True if job was found and updated
        """
        with self._jobs_lock:
            job = self._jobs.get(job_id)
            if not job:
                return False

            job.status = JobStatus.COMPLETED
            job.completed_at = datetime.utcnow()
            job.progress = 100
            job.result = result
            return True

    def fail_job(self, job_id: str, error: str) -> bool:
        """
        Mark a job as failed.

        Args:
            job_id: Job identifier
            error: Error message

        Returns:
            True if job was found and updated
        """
        with self._jobs_lock:
            job = self._jobs.get(job_id)
            if not job:
                return False

            job.status = JobStatus.FAILED
            job.completed_at = datetime.utcnow()
            job.error = error
            return True

    def cancel_job(self, job_id: str) -> bool:
        """
        Mark a job as cancelled.

        Args:
            job_id: Job identifier

        Returns:
            True if job was found and updated
        """
        with self._jobs_lock:
            job = self._jobs.get(job_id)
            if not job:
                return False

            job.status = JobStatus.CANCELLED
            job.completed_at = datetime.utcnow()
            return True

    # =========================================================================
    # Job Queries
    # =========================================================================

    def get_job(self, job_id: str) -> ExtractionJob | None:
        """
        Get a job by ID.

        Args:
            job_id: Job identifier

        Returns:
            The job or None if not found
        """
        with self._jobs_lock:
            return self._jobs.get(job_id)

    def get_job_result(self, job_id: str) -> dict[str, Any] | None:
        """
        Get job result data.

        Args:
            job_id: Job identifier

        Returns:
            The result data or None
        """
        with self._jobs_lock:
            job = self._jobs.get(job_id)
            return job.result if job else None

    def list_jobs(
        self,
        session_id: str | None = None,
        status: JobStatus | str | None = None,
        method: JobMethod | str | None = None,
        limit: int = 100,
    ) -> list[ExtractionJob]:
        """
        List jobs with optional filtering.

        Args:
            session_id: Filter by session (None for all)
            status: Filter by status
            method: Filter by method
            limit: Maximum jobs to return

        Returns:
            List of matching jobs
        """
        if isinstance(status, str):
            status = JobStatus(status)
        if isinstance(method, str):
            method = JobMethod(method)

        with self._jobs_lock:
            jobs: list[ExtractionJob] = []

            # Get job IDs to check
            if session_id:
                job_ids = self._sessions.get(session_id, set())
            else:
                job_ids = set(self._jobs.keys())

            for job_id in job_ids:
                job = self._jobs.get(job_id)
                if not job:
                    continue

                # Apply filters
                if status and job.status != status:
                    continue
                if method and job.method != method:
                    continue

                jobs.append(job)

                if len(jobs) >= limit:
                    break

            # Sort by created_at descending
            jobs.sort(key=lambda j: j.created_at, reverse=True)
            return jobs

    def get_session_jobs(self, session_id: str) -> list[ExtractionJob]:
        """
        Get all jobs for a session.

        Args:
            session_id: Session identifier

        Returns:
            List of jobs for the session
        """
        return self.list_jobs(session_id=session_id)

    # =========================================================================
    # Session Management
    # =========================================================================

    def get_active_sessions(self) -> list[str]:
        """
        Get list of active session IDs.

        Returns:
            List of session IDs with at least one job
        """
        with self._jobs_lock:
            return list(self._sessions.keys())

    def get_session_stats(self, session_id: str) -> dict[str, int]:
        """
        Get job statistics for a session.

        Args:
            session_id: Session identifier

        Returns:
            Dictionary with job counts by status
        """
        with self._jobs_lock:
            job_ids = self._sessions.get(session_id, set())
            stats = {status.value: 0 for status in JobStatus}

            for job_id in job_ids:
                job = self._jobs.get(job_id)
                if job:
                    stats[job.status.value] += 1

            return stats

    # =========================================================================
    # Cleanup
    # =========================================================================

    def cleanup_old_jobs(self, max_age_seconds: int | None = None) -> int:
        """
        Remove jobs older than max_age_seconds.

        Args:
            max_age_seconds: Maximum job age (default: self._job_ttl)

        Returns:
            Number of jobs removed
        """
        max_age = max_age_seconds or self._job_ttl
        cutoff = datetime.utcnow() - timedelta(seconds=max_age)
        removed = 0

        with self._jobs_lock:
            job_ids_to_remove: list[str] = []

            for job_id, job in self._jobs.items():
                if job.created_at < cutoff:
                    job_ids_to_remove.append(job_id)

            for job_id in job_ids_to_remove:
                if job_id in self._jobs:
                    job = self._jobs.pop(job_id)
                    # Remove from session tracking
                    session_jobs = self._sessions.get(job.session_id)
                    if session_jobs:
                        session_jobs.discard(job_id)
                        if not session_jobs:
                            del self._sessions[job.session_id]
                    removed += 1

        if removed > 0:
            logger.info(f"Cleaned up {removed} old jobs")

        return removed

    def delete_job(self, job_id: str) -> bool:
        """
        Delete a specific job.

        Args:
            job_id: Job identifier

        Returns:
            True if job was found and deleted
        """
        with self._jobs_lock:
            job = self._jobs.pop(job_id, None)
            if not job:
                return False

            # Remove from session tracking
            session_jobs = self._sessions.get(job.session_id)
            if session_jobs:
                session_jobs.discard(job_id)
                if not session_jobs:
                    del self._sessions[job.session_id]

            return True

    def clear_session(self, session_id: str) -> int:
        """
        Delete all jobs for a session.

        Args:
            session_id: Session identifier

        Returns:
            Number of jobs removed
        """
        with self._jobs_lock:
            job_ids = self._sessions.pop(session_id, set())
            removed = 0

            for job_id in job_ids:
                if job_id in self._jobs:
                    del self._jobs[job_id]
                    removed += 1

            return removed


# Singleton instance
_session_manager: SessionManager | None = None


def get_session_manager() -> SessionManager:
    """Get the singleton SessionManager instance."""
    global _session_manager
    if _session_manager is None:
        _session_manager = SessionManager()
    return _session_manager
