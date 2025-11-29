# Logging System Architecture

This directory contains the refactored logging system that follows the Single Responsibility Principle (SRP).

## Module Structure

```
logging/
├── LogStore.ts              - Core log storage and subscription management
├── LogFilter.ts             - Log filtering logic
├── LogFormatter.ts          - Log formatting and clipboard operations
├── TimestampFormatter.ts    - Timestamp formatting utilities
├── ImageLogHandler.ts       - Image recognition log processing
└── index.ts                 - Barrel exports
```

## Responsibilities

### LogStore

**Single Responsibility:** Managing in-memory storage of log entries

- Stores general logs and image recognition logs
- Manages observer pattern for change notifications
- Provides CRUD operations for logs
- Handles log counts

### LogFilter

**Single Responsibility:** Filtering log entries by criteria

- Filters logs by level (info, warning, error, debug, success)
- Supports "all" to return unfiltered logs
- Pure function approach - no side effects

### LogFormatter

**Single Responsibility:** Formatting logs for output

- Formats general logs as text
- Formats image recognition logs as text
- Formats action logs as text
- Handles clipboard operations
- Supports multiple log types (general, image, actions)

### TimestampFormatter

**Single Responsibility:** Formatting timestamps consistently

- Formats current time as HH:MM:SS
- Formats Unix timestamps as HH:MM:SS
- Ensures consistent timestamp format across the system

### ImageLogHandler

**Single Responsibility:** Processing image recognition events

- Parses raw image recognition data
- Extracts and formats location information
- Calculates match quality metrics (gap, percentOff)
- Extracts node IDs from hierarchy data
- Creates structured ImageRecognitionEntry objects

## LogManager (Coordinator)

The `LogManager` in the parent directory acts as a **facade** that:

- Coordinates between all specialized modules
- Provides a unified public API
- Handles log deduplication
- Maintains backward compatibility

## Benefits of This Architecture

1. **Single Responsibility Principle:** Each module has one clear responsibility
2. **Testability:** Each module can be tested in isolation
3. **Maintainability:** Changes to one concern don't affect others
4. **Reusability:** Modules can be used independently if needed
5. **Clear Dependencies:** Easy to understand what depends on what

## Usage Example

```typescript
import { logManager } from "./managers";

// Add a log (LogManager delegates to LogStore and TimestampFormatter)
logManager.addLog("info", "Application started");

// Get filtered logs (LogManager delegates to LogStore and LogFilter)
const errorLogs = logManager.getFilteredLogs("error");

// Copy logs (LogManager delegates to LogStore and LogFormatter)
await logManager.copyLogs("general");

// Process image recognition (LogManager delegates to ImageLogHandler)
logManager.processImageRecognitionData(eventData);
```

## Public API Stability

The public API exposed by `LogManager` remains unchanged, ensuring:

- Existing hooks (`useLogManager`) continue to work
- No breaking changes to components
- Backward compatibility with all consumers
