//! Core types for concurrency analysis
//!
//! Defines the data structures used across all race condition analyzers.

use serde::{Deserialize, Serialize};

/// Severity of a detected race condition
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RaceSeverity {
    /// Critical: Write-write race or read-write race with high confidence
    Critical,
    /// High: TOCTOU or unprotected compound operation
    High,
    /// Medium: Double-checked locking or partially protected state
    Medium,
    /// Low: Potential issue but likely false positive
    Low,
}

impl RaceSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            RaceSeverity::Critical => "critical",
            RaceSeverity::High => "high",
            RaceSeverity::Medium => "medium",
            RaceSeverity::Low => "low",
        }
    }
}

/// Type of state access
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessType {
    /// Read access
    Read,
    /// Write access
    Write,
    /// Compound operation (read-modify-write, e.g., +=)
    Compound,
}

/// Information about accessing shared state
#[derive(Debug, Clone)]
pub struct StateAccess {
    /// Name of the state being accessed
    pub state_name: String,
    /// Type of access
    pub access_type: AccessType,
    /// Line number where access occurs
    pub line: u32,
    /// Column number (if available)
    pub column: Option<u32>,
    /// Whether this access is protected by a lock
    pub is_protected: bool,
    /// Name of the protecting lock (if any)
    pub lock_name: Option<String>,
    /// Additional context about the access
    pub context: Option<String>,
}

/// Information about detected shared state
#[derive(Debug, Clone)]
pub struct SharedState {
    /// Name of the shared state
    pub name: String,
    /// Type of the state (class variable, module global, etc.)
    pub state_type: SharedStateType,
    /// Line where the state is defined
    pub definition_line: u32,
    /// File where the state is defined
    pub file: String,
    /// All accesses to this state
    pub accesses: Vec<StateAccess>,
    /// Whether this state appears to be thread-safe
    pub appears_thread_safe: bool,
    /// Reason why it appears thread-safe (if applicable)
    pub thread_safe_reason: Option<String>,
}

/// Type of shared state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedStateType {
    // Python
    ClassVariable,
    ModuleGlobal,
    ClosureCapture,
    InstanceAttribute,

    // Rust
    StaticMut,
    LazyStatic,
    OnceCell,
    ArcMutex,
    ArcRwLock,

    // TypeScript
    ModuleVariable,
    ClassProperty,
    ClosureVariable,

    // Generic
    Other(String),
}

impl SharedStateType {
    pub fn as_str(&self) -> &str {
        match self {
            SharedStateType::ClassVariable => "class_variable",
            SharedStateType::ModuleGlobal => "module_global",
            SharedStateType::ClosureCapture => "closure_capture",
            SharedStateType::InstanceAttribute => "instance_attribute",
            SharedStateType::StaticMut => "static_mut",
            SharedStateType::LazyStatic => "lazy_static",
            SharedStateType::OnceCell => "once_cell",
            SharedStateType::ArcMutex => "arc_mutex",
            SharedStateType::ArcRwLock => "arc_rwlock",
            SharedStateType::ModuleVariable => "module_variable",
            SharedStateType::ClassProperty => "class_property",
            SharedStateType::ClosureVariable => "closure_variable",
            SharedStateType::Other(s) => s,
        }
    }
}

/// Race condition pattern types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RacePattern {
    /// Write-write race: multiple unprotected writes to same state
    WriteWriteRace,
    /// Read-write race: unprotected write with concurrent reads
    ReadWriteRace,
    /// Time-of-check to time-of-use (e.g., if x not in dict: dict[x] = val)
    Toctou,
    /// Unprotected compound operation (+=, -=, etc.)
    UnprotectedCompound,
    /// Double-checked locking pattern
    DoubleCheckedLocking,
    /// Partially protected state (some accesses protected, some not)
    PartiallyProtected,
}

impl RacePattern {
    pub fn code(&self) -> &'static str {
        match self {
            RacePattern::WriteWriteRace => "RACE001",
            RacePattern::ReadWriteRace => "RACE002",
            RacePattern::Toctou => "RACE003",
            RacePattern::UnprotectedCompound => "RACE004",
            RacePattern::DoubleCheckedLocking => "RACE005",
            RacePattern::PartiallyProtected => "RACE006",
        }
    }

    pub fn default_severity(&self) -> RaceSeverity {
        match self {
            RacePattern::WriteWriteRace => RaceSeverity::Critical,
            RacePattern::ReadWriteRace => RaceSeverity::Critical,
            RacePattern::Toctou => RaceSeverity::High,
            RacePattern::UnprotectedCompound => RaceSeverity::High,
            RacePattern::DoubleCheckedLocking => RaceSeverity::Medium,
            RacePattern::PartiallyProtected => RaceSeverity::Medium,
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            RacePattern::WriteWriteRace => {
                "Multiple unprotected writes to the same state from different contexts"
            }
            RacePattern::ReadWriteRace => {
                "Unprotected write with concurrent reads from different contexts"
            }
            RacePattern::Toctou => "Time-of-check to time-of-use pattern (check-then-act)",
            RacePattern::UnprotectedCompound => {
                "Unprotected compound operation (read-modify-write)"
            }
            RacePattern::DoubleCheckedLocking => {
                "Double-checked locking pattern may be incorrect"
            }
            RacePattern::PartiallyProtected => {
                "Some accesses to state are protected, others are not"
            }
        }
    }
}

/// A detected race condition issue
#[derive(Debug, Clone)]
pub struct RaceIssue {
    /// Pattern type detected
    pub pattern: RacePattern,
    /// Severity level
    pub severity: RaceSeverity,
    /// File where the issue was detected
    pub file: String,
    /// Line number of the primary issue location
    pub line: u32,
    /// Column (if available)
    pub column: Option<u32>,
    /// Name of the shared state involved
    pub state_name: String,
    /// Description of the issue
    pub message: String,
    /// Related locations (other accesses)
    pub related_locations: Vec<(String, u32)>,
    /// Confidence level (0.0 - 1.0)
    pub confidence: f64,
}

/// Lock detection information
#[derive(Debug, Clone)]
pub struct LockInfo {
    /// Name of the lock variable
    pub name: String,
    /// Type of lock
    pub lock_type: LockType,
    /// Line where the lock is defined
    pub definition_line: u32,
    /// State variables this lock appears to protect
    pub protects: Vec<String>,
}

/// Type of synchronization lock
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockType {
    // Python
    ThreadingLock,
    ThreadingRLock,
    ThreadingSemaphore,
    AsyncioLock,

    // Rust
    StdMutex,
    StdRwLock,
    ParkingLotMutex,
    TokioMutex,

    // TypeScript
    CustomMutex,

    // Generic
    Other(String),
}

/// Analysis context for tracking state across a file
#[derive(Debug, Default)]
pub struct AnalysisContext {
    /// Detected shared states
    pub shared_states: Vec<SharedState>,
    /// Detected locks
    pub locks: Vec<LockInfo>,
    /// Detected issues
    pub issues: Vec<RaceIssue>,
    /// Current scope depth
    pub scope_depth: u32,
    /// Whether we're inside a lock context
    pub in_lock_context: bool,
    /// Current lock name (if in lock context)
    pub current_lock: Option<String>,
}

impl AnalysisContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Find a shared state by name
    pub fn find_state(&self, name: &str) -> Option<&SharedState> {
        self.shared_states.iter().find(|s| s.name == name)
    }

    /// Find a shared state by name (mutable)
    pub fn find_state_mut(&mut self, name: &str) -> Option<&mut SharedState> {
        self.shared_states.iter_mut().find(|s| s.name == name)
    }

    /// Add an access to a known shared state
    pub fn add_access(&mut self, state_name: &str, access: StateAccess) {
        if let Some(state) = self.find_state_mut(state_name) {
            state.accesses.push(access);
        }
    }

    /// Check if a state name matches a lock name pattern
    pub fn has_associated_lock(&self, state_name: &str) -> bool {
        let lock_patterns = [
            format!("{}_lock", state_name),
            format!("{}Lock", state_name),
            format!("{}_mutex", state_name),
            format!("{}Mutex", state_name),
        ];

        self.locks.iter().any(|lock| {
            lock_patterns.iter().any(|pattern| lock.name == *pattern)
                || lock.protects.contains(&state_name.to_string())
        })
    }
}
