# Contributing

Patches are welcome. Two things to know before you send one.

## The CLA

Every pull request needs a signed Contributor License Agreement. A bot comments on your
first PR with a link; signing is one click and once per person.

It exists for a specific reason, and it is worth stating plainly rather than burying. The
agent is AGPL and the SDK is permissive, and that split may need adjusting as the project
grows — dual-licensing for an organisation whose policy forbids AGPL, for instance. Changing
license terms requires permission from everyone who owns copyright in the code. Without a
CLA that means tracking down every contributor who ever landed a patch, including the ones
who have moved on and stopped answering email. In practice, projects in that position never
change their terms again.

The CLA does not take your copyright away. You keep it, and you grant the maintainer the
right to license your contribution under the project's terms, including future ones.

If you would rather not sign, open an issue describing the change instead. A clear bug
report is worth more than a patch nobody can accept.

## Which license your change lands under

The repository is not one license, because the parts are not one kind of thing.

| Directory | License |
| :--- | :--- |
| `src/`, the agent itself | AGPL-3.0 |
| `jumabek_sdk/` | MIT OR Apache-2.0 |
| `skills/` | MIT |

A change to the SDK or to a skill is permissively licensed; a change to the agent is AGPL.
The boundary follows the process boundary that already exists in the design: skills are
separate programs speaking a protocol over stdin and stdout.

## Before you open the PR

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same three on Linux, Windows and macOS, so running them locally is only about
finding out sooner.

A few things the codebase cares about:

- **Tests are named as sentences.** `a_skill_that_lies_about_its_methods_is_refused`, not
  `test_validator_3`. The name is the specification; the body is the proof.
- **A test should fail for one reason.** If you cannot say in one line what breaking it would
  mean, it is testing too much.
- **Skills speak the protocol, nothing else.** stdout belongs to the protocol; diagnostics go
  to stderr. A skill that prints to stdout corrupts the line the core is parsing.
