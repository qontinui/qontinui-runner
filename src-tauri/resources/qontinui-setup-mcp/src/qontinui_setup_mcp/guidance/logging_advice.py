"""Framework-specific instructions for setting up file-based logging.

Many frameworks only log to stdout by default, so users need to configure
them to write to files for the qontinui-runner to read.
"""

from __future__ import annotations

from typing import Any

LOGGING_ADVICE: dict[str, dict[str, Any]] = {
    "nextjs": {
        "framework": "Next.js",
        "logs_to_file_by_default": False,
        "summary": "Next.js logs to stdout by default. Use a process manager or redirect output.",
        "options": [
            {
                "method": "Process manager redirect",
                "difficulty": "easy",
                "description": "Redirect stdout/stderr when starting the dev server",
                "instructions": (
                    "Use a script or process manager to redirect output:\n"
                    "\n"
                    "```bash\n"
                    "next dev > logs/next.log 2>&1\n"
                    "```\n"
                    "\n"
                    "Or in package.json:\n"
                    "```json\n"
                    '"scripts": {\n'
                    '  "dev:log": "next dev 2>&1 | tee logs/next.log"\n'
                    "}\n"
                    "```"
                ),
            },
            {
                "method": "Winston logger",
                "difficulty": "moderate",
                "description": (
                    "Add Winston for structured file logging in API routes"
                    " and server components"
                ),
                "instructions": (
                    "Install Winston:\n"
                    "```bash\n"
                    "npm install winston\n"
                    "```\n"
                    "\n"
                    "Create a logger utility (lib/logger.ts):\n"
                    "```typescript\n"
                    "import winston from 'winston';\n"
                    "\n"
                    "export const logger = winston.createLogger({\n"
                    "  level: 'info',\n"
                    "  format: winston.format.combine(\n"
                    "    winston.format.timestamp(),\n"
                    "    winston.format.json()\n"
                    "  ),\n"
                    "  transports: [\n"
                    "    new winston.transports.File({ filename: 'logs/error.log', level: 'error' }),\n"
                    "    new winston.transports.File({ filename: 'logs/app.log' }),\n"
                    "  ],\n"
                    "});\n"
                    "```\n"
                    "\n"
                    "Use in API routes and server code."
                ),
            },
        ],
    },
    "react_vite": {
        "framework": "React/Vite",
        "logs_to_file_by_default": False,
        "summary": "Vite dev server logs to stdout. Redirect output or use a logging library.",
        "options": [
            {
                "method": "Process redirect",
                "difficulty": "easy",
                "description": "Redirect Vite dev server output to a file",
                "instructions": (
                    "```bash\n"
                    "vite dev > logs/vite.log 2>&1\n"
                    "```\n"
                    "\n"
                    "Or with tee:\n"
                    "```bash\n"
                    "vite dev 2>&1 | tee logs/vite.log\n"
                    "```"
                ),
            },
        ],
    },
    "express": {
        "framework": "Express",
        "logs_to_file_by_default": False,
        "summary": "Express logs to stdout by default. Use Morgan + Winston for file logging.",
        "options": [
            {
                "method": "Morgan + Winston",
                "difficulty": "moderate",
                "description": (
                    "Use Morgan for HTTP request logging and Winston for"
                    " application logs, both writing to files"
                ),
                "instructions": (
                    "Install packages:\n"
                    "```bash\n"
                    "npm install winston morgan\n"
                    "```\n"
                    "\n"
                    "Setup:\n"
                    "```javascript\n"
                    "const winston = require('winston');\n"
                    "const morgan = require('morgan');\n"
                    "const fs = require('fs');\n"
                    "const path = require('path');\n"
                    "\n"
                    "const logger = winston.createLogger({\n"
                    "  transports: [\n"
                    "    new winston.transports.File({ filename: 'logs/error.log', level: 'error' }),\n"
                    "    new winston.transports.File({ filename: 'logs/app.log' }),\n"
                    "  ],\n"
                    "});\n"
                    "\n"
                    "const accessLogStream = fs.createWriteStream(path.join(__dirname, 'logs/access.log'), { flags: 'a' });\n"
                    "app.use(morgan('combined', { stream: accessLogStream }));\n"
                    "```"
                ),
            },
        ],
    },
    "nestjs": {
        "framework": "NestJS",
        "logs_to_file_by_default": False,
        "summary": "NestJS logs to stdout via its built-in logger. Use nest-winston for file output.",
        "options": [
            {
                "method": "nest-winston",
                "difficulty": "moderate",
                "description": "Replace the built-in logger with Winston via nest-winston",
                "instructions": (
                    "Install:\n"
                    "```bash\n"
                    "npm install nest-winston winston\n"
                    "```\n"
                    "\n"
                    "In main.ts:\n"
                    "```typescript\n"
                    "import { WinstonModule } from 'nest-winston';\n"
                    "import * as winston from 'winston';\n"
                    "\n"
                    "const app = await NestFactory.create(AppModule, {\n"
                    "  logger: WinstonModule.createLogger({\n"
                    "    transports: [\n"
                    "      new winston.transports.File({ filename: 'logs/error.log', level: 'error' }),\n"
                    "      new winston.transports.File({ filename: 'logs/app.log' }),\n"
                    "    ],\n"
                    "  }),\n"
                    "});\n"
                    "```"
                ),
            },
        ],
    },
    "django": {
        "framework": "Django",
        "logs_to_file_by_default": False,
        "summary": "Django uses Python's logging module. Configure file handlers in settings.py.",
        "options": [
            {
                "method": "Django LOGGING setting",
                "difficulty": "easy",
                "description": "Add file handlers to Django's LOGGING configuration",
                "instructions": (
                    "In settings.py:\n"
                    "```python\n"
                    "LOGGING = {\n"
                    "    'version': 1,\n"
                    "    'disable_existing_loggers': False,\n"
                    "    'handlers': {\n"
                    "        'file': {\n"
                    "            'level': 'INFO',\n"
                    "            'class': 'logging.FileHandler',\n"
                    "            'filename': 'logs/django.log',\n"
                    "        },\n"
                    "        'error_file': {\n"
                    "            'level': 'ERROR',\n"
                    "            'class': 'logging.FileHandler',\n"
                    "            'filename': 'logs/error.log',\n"
                    "        },\n"
                    "    },\n"
                    "    'loggers': {\n"
                    "        'django': {\n"
                    "            'handlers': ['file', 'error_file'],\n"
                    "            'level': 'INFO',\n"
                    "            'propagate': True,\n"
                    "        },\n"
                    "    },\n"
                    "}\n"
                    "```\n"
                    "\n"
                    "Create the logs directory:\n"
                    "```bash\n"
                    "mkdir -p logs\n"
                    "```"
                ),
            },
        ],
    },
    "flask": {
        "framework": "Flask",
        "logs_to_file_by_default": False,
        "summary": "Flask logs to stdout. Add a file handler to the Flask app logger.",
        "options": [
            {
                "method": "Python logging FileHandler",
                "difficulty": "easy",
                "description": "Add a RotatingFileHandler to Flask's logger",
                "instructions": (
                    "```python\n"
                    "import logging\n"
                    "from logging.handlers import RotatingFileHandler\n"
                    "\n"
                    "handler = RotatingFileHandler('logs/flask.log', maxBytes=10_000_000, backupCount=5)\n"
                    "handler.setLevel(logging.INFO)\n"
                    "app.logger.addHandler(handler)\n"
                    "```"
                ),
            },
        ],
    },
    "fastapi": {
        "framework": "FastAPI",
        "logs_to_file_by_default": False,
        "summary": "FastAPI/Uvicorn logs to stdout. Use structlog or Python logging for file output.",
        "options": [
            {
                "method": "structlog + file handler",
                "difficulty": "moderate",
                "description": "Use structlog for structured JSON file logging",
                "instructions": (
                    "Install:\n"
                    "```bash\n"
                    "pip install structlog\n"
                    "```\n"
                    "\n"
                    "Setup:\n"
                    "```python\n"
                    "import structlog\n"
                    "import logging\n"
                    "\n"
                    "logging.basicConfig(\n"
                    "    filename='logs/app.log',\n"
                    "    level=logging.INFO,\n"
                    "    format='%(message)s',\n"
                    ")\n"
                    "\n"
                    "structlog.configure(\n"
                    "    processors=[\n"
                    "        structlog.processors.TimeStamper(fmt='iso'),\n"
                    "        structlog.processors.JSONRenderer(),\n"
                    "    ],\n"
                    "    logger_factory=structlog.stdlib.LoggerFactory(),\n"
                    ")\n"
                    "```"
                ),
            },
            {
                "method": "Uvicorn log config",
                "difficulty": "easy",
                "description": "Configure Uvicorn to write access logs to a file",
                "instructions": (
                    "Run with log file:\n"
                    "```bash\n"
                    "uvicorn app:app --log-config logging.yaml\n"
                    "```\n"
                    "\n"
                    "Or redirect:\n"
                    "```bash\n"
                    "uvicorn app:app 2>&1 | tee logs/uvicorn.log\n"
                    "```"
                ),
            },
        ],
    },
    "rust_cargo": {
        "framework": "Rust/Cargo",
        "logs_to_file_by_default": False,
        "summary": "Rust programs use the tracing or log crate. Add a file subscriber/appender.",
        "options": [
            {
                "method": "tracing-appender",
                "difficulty": "moderate",
                "description": "Use tracing + tracing-appender for file logging",
                "instructions": (
                    "Add to Cargo.toml:\n"
                    "```toml\n"
                    "[dependencies]\n"
                    'tracing = "0.1"\n'
                    'tracing-subscriber = "0.3"\n'
                    'tracing-appender = "0.2"\n'
                    "```\n"
                    "\n"
                    "Setup:\n"
                    "```rust\n"
                    "use tracing_appender::rolling;\n"
                    "use tracing_subscriber::fmt;\n"
                    "\n"
                    'let file_appender = rolling::daily("logs", "app.log");\n'
                    "let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);\n"
                    "\n"
                    "fmt().with_writer(non_blocking).init();\n"
                    "```"
                ),
            },
        ],
    },
    "go": {
        "framework": "Go",
        "logs_to_file_by_default": False,
        "summary": "Go's log package writes to stderr by default. Redirect or use a structured logger.",
        "options": [
            {
                "method": "log.SetOutput",
                "difficulty": "easy",
                "description": "Set the log package output to a file",
                "instructions": (
                    "```go\n"
                    'f, err := os.OpenFile("logs/app.log", os.O_RDWR|os.O_CREATE|os.O_APPEND, 0666)\n'
                    "if err != nil {\n"
                    "    log.Fatal(err)\n"
                    "}\n"
                    "defer f.Close()\n"
                    "log.SetOutput(f)\n"
                    "```"
                ),
            },
            {
                "method": "zerolog or zap",
                "difficulty": "moderate",
                "description": "Use a structured logging library with file output",
                "instructions": (
                    "With zerolog:\n"
                    "```go\n"
                    "import (\n"
                    '    "os"\n'
                    '    "github.com/rs/zerolog"\n'
                    ")\n"
                    "\n"
                    'f, _ := os.Create("logs/app.log")\n'
                    "logger := zerolog.New(f).With().Timestamp().Logger()\n"
                    "```"
                ),
            },
        ],
    },
    "spring_boot": {
        "framework": "Spring Boot",
        "logs_to_file_by_default": True,
        "summary": "Spring Boot logs to files by default via Logback. Check application.properties for log path.",
        "options": [
            {
                "method": "application.properties",
                "difficulty": "easy",
                "description": "Configure log file path in application.properties",
                "instructions": (
                    "In application.properties:\n"
                    "```properties\n"
                    "logging.file.name=logs/spring.log\n"
                    "logging.level.root=INFO\n"
                    "```"
                ),
            },
        ],
    },
    "rails": {
        "framework": "Rails",
        "logs_to_file_by_default": True,
        "summary": "Rails logs to log/ directory by default (development.log, production.log, test.log).",
        "options": [
            {
                "method": "Default (already configured)",
                "difficulty": "easy",
                "description": "Rails already writes to log/ directory. Just point the runner at it.",
                "instructions": (
                    "Rails logs are at:\n"
                    "- `log/development.log` (development)\n"
                    "- `log/production.log` (production)\n"
                    "- `log/test.log` (test)\n"
                    "\n"
                    "No additional configuration needed."
                ),
            },
        ],
    },
    "tauri": {
        "framework": "Tauri",
        "logs_to_file_by_default": False,
        "summary": "Tauri apps log via the tracing crate. Add tracing-appender for file output.",
        "options": [
            {
                "method": "tracing-appender",
                "difficulty": "moderate",
                "description": "Same as Rust/Cargo -- use tracing-appender",
                "instructions": (
                    "See Rust/Cargo logging advice. Additionally, Tauri's plugin-log"
                    " can write to the app data directory:\n"
                    "\n"
                    "```rust\n"
                    "// In Cargo.toml:\n"
                    '// tauri-plugin-log = "2"\n'
                    "\n"
                    "use tauri_plugin_log::{Target, TargetKind};\n"
                    "\n"
                    "tauri::Builder::default()\n"
                    "    .plugin(tauri_plugin_log::Builder::new()\n"
                    '        .targets([Target::new(TargetKind::LogDir { file_name: Some("app".into()) })])\n'
                    "        .build())\n"
                    "```"
                ),
            },
        ],
    },
    "react_native": {
        "framework": "React Native",
        "logs_to_file_by_default": False,
        "summary": (
            "React Native logs via Metro bundler to stdout."
            " Use react-native-fs for on-device file logging."
        ),
        "options": [
            {
                "method": "Metro output redirect",
                "difficulty": "easy",
                "description": "Redirect Metro bundler output to a file",
                "instructions": (
                    "```bash\nnpx react-native start 2>&1 | tee logs/metro.log\n```"
                ),
            },
        ],
    },
    "flutter": {
        "framework": "Flutter",
        "logs_to_file_by_default": False,
        "summary": (
            "Flutter logs via dart:developer to the console."
            " Use a logging package for file output."
        ),
        "options": [
            {
                "method": "Flutter run redirect",
                "difficulty": "easy",
                "description": "Redirect Flutter run output to a file",
                "instructions": (
                    "```bash\nflutter run 2>&1 | tee logs/flutter.log\n```"
                ),
            },
        ],
    },
}


async def get_logging_advice(framework: str) -> dict[str, Any]:
    """Get framework-specific logging advice.

    Args:
        framework: framework key (e.g. "nextjs", "fastapi", "django")

    Returns:
        The advice dict for that framework, or an error dict if framework
        not found. Includes an "available_frameworks" list in the error case.
    """
    advice = LOGGING_ADVICE.get(framework)
    if advice is not None:
        return advice

    return {
        "error": f"No logging advice found for framework: {framework}",
        "available_frameworks": sorted(LOGGING_ADVICE.keys()),
    }
