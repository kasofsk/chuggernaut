# StudyBuddy

Korean language learning mobile app built with Flutter. Uses Pimsleur-style spaced repetition drills with voice input/output.

## Tech Stack

- **Framework:** Flutter (iOS + Android)
- **Language:** Dart
- **State management:** Riverpod (manual providers — do NOT use `riverpod_generator`, it conflicts with `drift_dev`)
- **Database:** Drift (type-safe SQLite wrapper) with `sqlite3_flutter_libs`
- **Models:** Freezed for immutable data classes, `json_serializable` for JSON
- **Routing:** go_router (declarative)
- **HTTP:** dio
- **Code generation:** `dart run build_runner build --delete-conflicting-outputs`

## Architecture

```
lib/
├── main.dart / app.dart
├── core/           # Theme, router, shared config
├── data/           # Database, repositories
│   ├── database/   # Drift tables and DAOs
│   └── repositories/
├── domain/         # Models (freezed) and service interfaces
│   ├── models/
│   └── services/
└── features/       # Screen-level UI organized by feature
    ├── home/
    ├── drill/
    ├── lessons/
    └── progress/
```

## Key Commands

```bash
flutter pub get                              # Install dependencies
flutter analyze                              # Static analysis
flutter test                                 # Run tests
dart run build_runner build --delete-conflicting-outputs  # Regenerate freezed/drift/json code
flutter build apk --debug                    # Android debug build
```

## Conventions

- Riverpod providers are written manually (no codegen) to avoid analyzer conflicts with Drift
- Models use `@freezed` annotation — always regenerate after changing model files
- Drift tables defined in `lib/data/database/` — regenerate after schema changes
- iOS builds require macOS + Xcode (not available in CI Docker containers)
- CI validation focuses on `flutter analyze` + `flutter test` + Android builds

## Backend (Phase 2+)

Tickets 10+ introduce a Python backend:
- **Framework:** FastAPI
- **Database:** PostgreSQL
- **Task queue:** Celery + Redis
- **Location:** `backend/` subdirectory

Backend requires these env vars (provided as secrets):
- `ANTHROPIC_API_KEY` — Claude API for drill orchestration and content extraction
- `OPENAI_API_KEY` — Whisper API for audio transcription
- `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` — S3 for audio file storage
- `DATABASE_URL` — PostgreSQL connection string
