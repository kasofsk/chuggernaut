# Bauhaus Pomodoro

A Pomodoro timer app with a Bauhaus/Constructivist design system. Sharp geometry, heavy lines, intentional asymmetry — no soft edges.

## Tech Stack

- **Framework:** Flutter (iOS + Android)
- **Language:** Dart
- **State management:** Riverpod (manual providers — do NOT use `riverpod_generator`, it conflicts with `drift_dev`)
- **Database:** Drift (type-safe SQLite wrapper) with `sqlite3_flutter_libs`
- **Models:** Freezed for immutable data classes, `json_serializable` for JSON
- **Routing:** go_router (declarative)
- **Charts:** fl_chart (bar charts, no curved lines — Bauhaus aesthetic)
- **Fonts:** google_fonts — Space Grotesk (headlines) + Inter (body/labels)
- **Icons:** Material Symbols Outlined (2px+ weight, geometric strokes only)
- **Code generation:** `dart run build_runner build --delete-conflicting-outputs`

## Architecture

```
lib/
├── main.dart / app.dart
├── core/
│   ├── theme/
│   │   ├── bauhaus_theme.dart       # ThemeData with Bauhaus design tokens
│   │   ├── colors.dart              # Full Material3 color scheme
│   │   └── typography.dart          # Space Grotesk + Inter text themes
│   ├── router.dart                  # go_router: /timer, /tasks, /stats, /settings
│   └── widgets/
│       ├── ghost_block.dart         # Offset solid-color shadow wrapper
│       ├── bauhaus_app_bar.dart     # Fixed top bar, 4px bottom border
│       ├── bauhaus_nav_bar.dart     # Fixed bottom nav, 4px top border
│       ├── bauhaus_button.dart      # Sharp-cornered buttons with ghost-block hover
│       ├── bauhaus_toggle.dart      # Split-block toggle switch
│       └── heavy_divider.dart       # 2px/4px/8px solid dividers
├── data/
│   ├── database/
│   │   └── app_database.dart        # Drift tables + DAOs
│   └── repositories/
│       ├── task_repository.dart
│       ├── session_repository.dart
│       └── settings_repository.dart
├── domain/
│   ├── models/
│   │   ├── pomodoro_task.dart       # Task with priority, pomodoro estimate
│   │   ├── pomodoro_session.dart    # Completed session record
│   │   ├── timer_state.dart         # FSM: idle, focus, short_break, long_break
│   │   └── app_settings.dart        # User preferences (durations, theme variant)
│   └── services/
│       ├── timer_service.dart       # Countdown logic, session cycling
│       └── statistics_service.dart  # Aggregations for efficiency report
└── features/
    ├── timer/
    │   ├── timer_screen.dart
    │   └── widgets/
    │       ├── master_timer_circle.dart  # The one allowed circle
    │       ├── timer_controls.dart       # START + reset ghost-block buttons
    │       └── active_task_preview.dart
    ├── tasks/
    │   ├── tasks_screen.dart
    │   └── widgets/
    │       ├── staircase_grid.dart       # 12-col asymmetric task layout
    │       ├── task_card.dart            # Priority-colored block
    │       └── add_task_sheet.dart
    ├── stats/
    │   ├── efficiency_report_screen.dart
    │   └── widgets/
    │       ├── focus_distribution_chart.dart  # Weekly bar chart
    │       ├── tomato_grid.dart               # Circle grid visualization
    │       └── velocity_map.dart              # Asymmetric metric blocks
    └── settings/
        ├── settings_screen.dart
        └── widgets/
            ├── timer_parameters_section.dart
            ├── visual_interface_section.dart
            └── layout_selector.dart
```

## Design System: "The Functional Constructivist"

### Colors

| Token | Hex | Usage |
|-------|-----|-------|
| primary | `#b12c16` | Active state, primary CTAs, tomato red |
| primary-container | `#ff6347` | Timer circle fill, active task highlight |
| secondary | `#0001c0` | Navigation, secondary data, Bauhaus blue |
| tertiary | `#705d00` | Completed/break states, Bauhaus yellow |
| tertiary-container | `#c9a900` | Yellow accent blocks |
| on-surface | `#1b1b1b` | All text, structural borders |
| surface | `#f9f9f9` | Base canvas |
| surface-container-high | `#e8e8e8` | Elevated sections |
| surface-container-highest | `#e2e2e2` | Highest elevation blocks |

### Typography

- **Headlines/Display:** Space Grotesk — Black weight, uppercase, tight tracking (-0.02em)
- **Body:** Inter — Regular/Bold, sentence case
- **Labels/Metadata:** Inter — Bold, uppercase, wide tracking, 10-12px

### The Rules

1. **No border-radius.** Everything is sharp-cornered. The ONLY exception is perfect circles (`BorderRadius.circular(9999)`) for the timer and active nav indicator.
2. **No drop shadows.** Use "Ghost Blocks" — a solid `#1b1b1b` rectangle offset 4-6px behind the element. On press, the element translates into the ghost block.
3. **No 1px borders.** Minimum 2px. Structural dividers are 4px. Major section breaks are 8px.
4. **No soft greys.** Every neutral is warm-tinted or architectural.
5. **Asymmetric layouts.** Use 12-column grids with intentionally unequal column spans (7/5, 8/4, 10/2).

### Ghost Block Implementation

```dart
class GhostBlock extends StatelessWidget {
  final Widget child;
  final double offset;

  const GhostBlock({required this.child, this.offset = 4});

  @override
  Widget build(BuildContext context) {
    return Stack(
      children: [
        Positioned(
          top: offset, left: offset,
          child: Container(color: Color(0xFF1B1B1B), /* match child size */),
        ),
        child,
      ],
    );
  }
}
```

## Key Commands

```bash
flutter pub get                              # Install dependencies
flutter analyze                              # Static analysis
flutter test                                 # Run tests
dart run build_runner build --delete-conflicting-outputs  # Regenerate freezed/drift code
flutter build apk --debug                    # Android debug build
```

## Conventions

- Riverpod providers are written manually (no codegen) to avoid analyzer conflicts with Drift
- Models use `@freezed` annotation — always regenerate after changing model files
- Drift tables defined in `lib/data/database/` — regenerate after schema changes
- All UI text is UPPERCASE for headlines/labels (matches Bauhaus aesthetic)
- Use `Border.all(width: 2, color: theme.colorScheme.onSurface)` as the default border
- Ghost-block offset is 4px for cards, 6px for the timer circle
- Bar charts use flat tops, no curves, 2px borders — fl_chart with `isCurved: false`
- The bottom nav active indicator is a perfect circle with primary-container fill
