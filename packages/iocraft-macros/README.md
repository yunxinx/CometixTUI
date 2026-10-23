# iocraft-macros

This repository contains macro implementations for [`iocraft`](https://github.com/ccbrown/iocraft).

The macros also work when the `iocraft` dependency is renamed in `Cargo.toml`;
generated paths use the dependency name chosen by the consuming package.

`#[derive(Props)]` supports `#[iocraft(required)]` for fields that callers must
supply. `element!` initializes omitted optional fields using each field type's
`Default`; it does not use a handwritten `Default` on the props struct.

Go there to learn more!
