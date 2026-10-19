# acquire-run Specification

## Purpose
Defines how an `opcua-scan acquire` run ends and reports its outcome, and which timestamp each
acquired sample carries.

## Requirements

### Requirement: A timed run completes successfully
When continuous tasks end because `--duration` elapsed, the run SHALL finish normally and exit with code 0.

#### Scenario: Duration elapses
- **WHEN** `opcua-scan acquire --config acquire.toml --duration 2s` runs without errors
- **THEN** it stops after about 2 seconds and exits with code 0

### Requirement: An interrupted run reports the interruption
When the user interrupts a run (Ctrl+C / SIGINT), the run SHALL stop its tasks, flush the output and exit with code 4, also when `--duration` was given.

#### Scenario: Ctrl+C during a timed run
- **WHEN** a run with `--duration 1m` is interrupted after 5 seconds
- **THEN** it exits with code 4

### Requirement: Every sample has a time
Each sample SHALL carry a time: the value's source timestamp, else its server timestamp, else the time the client received the value, in RFC 3339.

#### Scenario: Server without timestamps
- **WHEN** a server returns a value with neither source nor server timestamp
- **THEN** the sample's `time` is the receive time, not null

#### Scenario: Source timestamp present
- **WHEN** the value has a source timestamp
- **THEN** the sample's `time` is that source timestamp
