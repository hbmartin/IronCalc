# Calculation fork

Base: upstream `eb9ca75bd99160c17902359d2d87225c4571c468` (0.9.1).

Iterative settings and the repeated-evaluation approach adapt the unmerged
[upstream PR #1466](https://github.com/ironcalc/IronCalc/pull/1466), by
SahilRajputdotexe. Upstream has not approved these semantics.

The fork preserves disabled/100/0.001 defaults and XLSX `calcPr` properties.
It validates settings, initializes iterations from zero, and exposes
`Model::evaluation_report()`. Consumers must reject unsuccessful reports;
reaching the iteration cap is not convergence. Cyclic nonnumeric and array
calculations are outside this fork's initial supported calculation mode.

The internal bitcode workbook format changes with calculation settings.
Use XLSX as the interchange format; do not assume old `.icalc` files load.

Only generic calculation changes and synthetic tests belong here. Application
code and proprietary financial workbook fixtures stay in the private backend.
