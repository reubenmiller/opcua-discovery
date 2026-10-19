# Design

## Decisions

- **One data source.** The bindings are a static table (`KEY_HELP`: group, key, legend label,
  description, priority). The legend and the overlay are both rendered from it, so they cannot
  drift apart; context-only entries (filter, prompt, overlay) are small separate lists.
- **Fitting** walks the context's entries by priority and keeps those whose cumulative width,
  plus the reserved `? help`, fits the row; the kept entries are then shown in table order.
- **Overlay** is a centered `Clear` + bordered `Paragraph`, drawn last. A `help_open` flag is
  checked first in `handle_key`, like `filter_mode` and `acquire_mode`.
- **Layout**: the legend takes one row between the panes and the progress/status rows.
- The default status becomes `ready — press ? for all keys`; the edit-config status keeps its
  "selection M/N located" text without key lists.
