# cargo-metest

cargo-metest is a replacement for `cargo test` which will run tests as jobs on a
distributed clustered job runner. Each test runs in a lightweight container
where it is isolated from computer it is running on and other tests.

Running your tests using it can be as simple as running `cargo metest` instead
of `cargo test`, (see [Running Tests](./cargo_metest/running_tests.md) for
details) but due to the tests running in a very isolated environment by default,
there can be some configuration required to make all your tests pass (see
[Configuration](./cargo_metest/configuration.md).)