# Rustanka patches

Source: crates.io `gtmpl-ng` 0.7.7, MIT license (see LICENSE).
Crate checksum: `5e2314ffd030e6c1ba91e9f586a9d57dc6a423cf0581b48840367938be014266`.

Vendored because the Helm benchmark needs Go-template reassignment (`$acc = ...`).
The lexer/parser distinguish assignment from declaration, and execution updates
the nearest existing variable. Range loops maintain a separate declaration scope,
support assignment targets, visit map keys in sorted order, and run `else` only
when the collection is empty. Rustanka regression tests cover these changes.
