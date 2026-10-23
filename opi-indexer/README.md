# opi-indexer

Rust workspace that links upstream `ord` 0.23.2 as a library.

## ord dependency

`ord` is a git dependency (`https://github.com/ordinals/ord`, tag `0.23.2`), the
same source the `mockcore` dev-dependency comes from. With the crates.io `ord`
alongside the git `mockcore`, Cargo builds two copies of `ord` and `ordinals`, so
their types do not unify. A single git source gives one copy of each.

`Cargo.lock` is seeded from ord 0.23.2's own lockfile: ord requires `bip322 =
0.0.9`, which crates.io has yanked, and Cargo only keeps a yanked version that is
already pinned in the lockfile. Keep that pin when updating the lockfile.
