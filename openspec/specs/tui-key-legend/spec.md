# tui-key-legend Specification

## Purpose
Makes every key of the interactive explorer discoverable without documentation: a key legend
that is always on screen and matches the current context, and a help overlay with all bindings.

## Requirements

### Requirement: Always-visible key legend
The TUI SHALL show a key legend row directly above the status line at all times. Each entry SHALL show the key and a short action (`space mark`). Status messages SHALL NOT replace the legend.

#### Scenario: After a status message
- **WHEN** the user expands a node and the status line reports the number of children
- **THEN** the legend row still lists the browsing keys

### Requirement: Legend follows the context
The legend SHALL list the keys of the current context: browsing (move, expand, collapse, mark, library, acquire, export, filter, value, vars-only, subscribe/poll marks, validate, quit), filter input (apply, clear, delete), the acquire prompt (the input format, write, cancel), and the help overlay (close). While a background job runs, the legend SHALL include `x cancel`.

#### Scenario: Typing a filter
- **WHEN** the user presses `/`
- **THEN** the legend shows `⏎ apply`, `Esc clear` and `⌫ delete` instead of the browsing keys

#### Scenario: Running export
- **WHEN** an export job is running
- **THEN** the legend includes `x cancel`

### Requirement: Legend fits the terminal width
When the legend does not fit the terminal width, entries SHALL be dropped from the lowest priority up, and `? help` SHALL always remain as the last entry.

#### Scenario: Narrow terminal
- **WHEN** the terminal is 60 columns wide
- **THEN** the legend shows the most important browsing keys that fit and ends with `? help`

### Requirement: Help overlay
Pressing `?` while browsing SHALL open an overlay listing every key binding grouped by purpose (navigation, marking, actions, other) with a description, and the meaning of the mark symbols `[x]`, `[S]`, `[P]` and `[+]`. `?`, `Esc` or `q` SHALL close it without other effect; `Ctrl+C` SHALL still quit; `↑`/`↓` SHALL scroll it when it is taller than the terminal; other keys SHALL be ignored while it is open.

#### Scenario: Open and close
- **WHEN** the user presses `?` and then `q`
- **THEN** the overlay closes and the TUI keeps running

#### Scenario: Small terminal
- **WHEN** the overlay is opened on an 80×24 terminal
- **THEN** its title says `↑↓ scroll` and `↓` reveals the remaining lines

#### Scenario: Keys are inert while open
- **WHEN** the overlay is open and the user presses `space`
- **THEN** no node is marked
