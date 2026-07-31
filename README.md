<img src="docs/banner.svg" alt="JumaBek" width="100%">

<p>
  <a href="../../actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/japo0nn/jumabek/ci.yml?branch=main&style=flat-square&label=ci&labelColor=1f2a37&color=a3d977"></a>
  <a href="../../releases/latest"><img alt="Release" src="https://img.shields.io/github/v/release/japo0nn/jumabek?style=flat-square&label=release&labelColor=1f2a37&color=5ccfe6"></a>
  <a href="../../releases/latest"><img alt="Downloads" src="https://img.shields.io/github/downloads/japo0nn/jumabek/total?style=flat-square&label=downloads&labelColor=1f2a37&color=f07178"></a>
  <img alt="Platforms" src="https://img.shields.io/badge/platform-windows%20%C2%B7%20linux%20%C2%B7%20macos-c3a6ff?style=flat-square&labelColor=1f2a37">
  <img alt="Rust" src="https://img.shields.io/badge/rust-2024%20edition-ffcc66?style=flat-square&labelColor=1f2a37">
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/github/license/japo0nn/jumabek?style=flat-square&label=license&labelColor=1f2a37&color=8aa0b8"></a>
</p>

**An assistant that writes its own skills when it runs out of them.**

It runs on your machine, does real work on it, and when a task needs something it cannot do,
it says so, asks, and compiles the missing piece — each one checked inside a container before
it is allowed near your files.

[Install](#install) · [How it works](#how-it-works) · [Writing skills](#writing-skills) · [Safety](#safety) · [Limits](#what-it-does-not-do) · [Site](https://japo0nn.github.io/jumabek/)

---

## A real session

Asked for something it has no skill for, JumaBek notices, explains what is missing, and asks
before writing anything.

```console
> I need to parse RSS feeds regularly and pull out the headlines

  I have shell_executor, but it only runs commands. Parsing RSS needs an HTTP
  client and an XML parser, which a shell does not have. I suggest building a
  skill called rss_parser. Shall I start?

  permission  MEDIUM   write a new skill 'rss_parser'
  Write, compile and install a new skill 'rss_parser'. The code is written by
  the model and compiled on this machine; once installed it loads in every
  future session.

  allow? [y/N] y
  allowed

  · rss_parser: preflight passed in docker engine 28.5.1 —
      build: 2 cpu / 2g ram, network on · run: 0.5 cpu / 256m, no network, read-only
  · rss_parser: built and validated
  · rss_parser is live: fetch_titles, fetch_titles_formatted

  · rss_parser · fetch_titles_formatted

  Here are the headlines from Hacker News: ...
```

Forty-nine seconds from the request to a working result. The skill stays installed and loads
on every later run.

---

## Install

<table>
<tr><td><b>Windows</b></td><td>

```powershell
irm https://raw.githubusercontent.com/japo0nn/jumabek/main/install.ps1 | iex
```

</td></tr>
<tr><td><b>Linux, macOS</b></td><td>

```bash
curl -fsSL https://raw.githubusercontent.com/japo0nn/jumabek/main/install.sh | bash
```

</td></tr>
</table>

The installer puts everything under `~/.jumabek`, adds it to your PATH, and never overwrites
a config you have already edited. It offers to set up a local LLM router, and does not do it
behind your back.

From source, if you would rather:

```bash
cargo install --git https://github.com/japo0nn/jumabek
```

Then set a key and start:

```bash
export JUMABEK_API_KEY="your-key"     # or put it in ~/.jumabek/secrets.toml
jumabek
```

### Upgrading

Re-run the installer. It replaces the binaries and leaves `config.toml`, `secrets.toml` and
`prompt.md` alone, because those are yours once they exist.

That last one has a consequence worth knowing. `prompt.md` is how the model learns what it
is allowed to do, so a release that adds an action adds it there too — and your copy will
not have it. The code ships, the capability stays invisible. If a new version announces
something the agent never seems to use, compare your `~/.jumabek/prompt.md` against the one
in the release archive.

### What it needs

| Dependency | Without it |
| :--- | :--- |
| An OpenAI-compatible endpoint | nothing works |
| Rust toolchain | it runs, but cannot write itself new skills |
| Docker | new skills are refused, because they cannot be checked first |
| ffmpeg | voice is unavailable; typing still works |

`jumabek doctor` reports all of it, and says what each gap costs you:

```console
  ok   home         ~/.jumabek
  ok   config       ~/.jumabek/config.toml
  ok   API key      found
  ok   LLM          http://localhost:20128/api · oc/big-pickle
  ok   Rust         cargo 1.96.0 — skills can be built
  WARN Docker       docker is installed but the engine is not running
       new skills are checked in a container before they touch your machine;
       without it building them is refused
  ok   skills       2 installed: shell_executor, rss_parser

  6 ok, 1 warning(s), 0 failure(s)
  JumaBek will run; the warnings above disable parts of it
```

> [!NOTE]
> JumaBek was developed and tested against
> [OmniRoute](https://www.npmjs.com/package/omniroute), a local router that puts many
> providers behind one OpenAI-compatible endpoint: `npm i -g omniroute && omniroute serve`.
> Any other OpenAI-compatible endpoint should work — the client sends `model`, `messages`
> and `stream`, and reads `choices[0].message.content` — but nothing else has been tested.

---

## How it works

Every skill is a separate process. JumaBek writes a line of JSON to its stdin and reads a
line back from its stdout.

```jsonc
// core  ->
{"id":1,"method":"execute","params":"{\"method\":\"run\",\"args\":\"ls\"}"}
// skill ->
{"id":1,"payload":{"Output":{"Text":"file1.txt\nfile2.txt"}}}
```

That is the whole contract, and it buys several things at once.

| | |
| :--- | :--- |
| **Any language** | A skill is whatever speaks the protocol. Rust, Python, Go — the agent never knows the difference. |
| **Nothing to rebuild** | Adding a skill means dropping a binary in a folder. The agent itself is never recompiled. |
| **Crashes stay local** | A skill that hangs is killed and restarted on the next call. It cannot take the agent down. |
| **Lazy by default** | Descriptions are cached, so twenty installed skills cost one millisecond at startup instead of seven hundred. |

### Memory

Everything said is kept in SQLite. The current session is always in context; older sessions
are searched only when the model asks, through a full-text index with Russian and English
stemming — so `файл` finds `файлами`, and `file` finds `files`.

When a conversation outgrows the context window, the oldest exchanges are dropped in whole
task groups — never half of one, which would leave a result with no matching command — and
replaced by a marker telling the model what it can still recall.

### Sub-agents

Some work is worth doing but not worth reading. Scanning forty log files fills a context
window with output whose only useful part is the conclusion.

So JumaBek can hand a piece of work to a copy of itself. The copy starts empty — the system
prompt, the skills, and a task written as a standalone instruction. It cannot see the
conversation it came from, which is the entire point. It runs its own loop and returns one
summary.

```
  · subagent · read every .log under C:/logs and list the error codes
  · shell_executor · run
  · subagent · done in 12.4s
```

Nesting stops at two levels. Below that, a tree is almost always a task that failed to
decompose and started looping on itself.

### Background jobs

A job is work that outlives the prompt: a reminder, a recurring check, a folder being
watched. Jobs live in SQLite and come back after a restart — most of what makes a reminder
worth setting.

| Schedule | Meaning |
| :--- | :--- |
| `in 3h` | once, three hours from now |
| `at 2026-07-30T09:00:00Z` | once, at a moment |
| `every 30m` | repeating, minimum 10s |
| `cron 0 9 * * 1-5` | five fields: minute hour day month weekday |
| `watch ~/Downloads` | when something there appears, changes or disappears |

Watching polls and compares name, size and modification time. Filesystem events would be
sharper, but they cost a dependency and a debouncing problem to save a few seconds on a job
that runs every quarter hour. The first look only learns what is there — otherwise every
watch would fire once at startup on a directory nobody touched.

Jobs report into the live session through rustyline's external printer, which redraws the
line you are typing underneath the message instead of through it.

---

## Writing skills

You can write one yourself, or let JumaBek do it. Either way it is one file.

```rust
use jumabek_sdk::{MethodInfo, ModuleMetadata, SkillError, SkillModule, SkillOutput};

struct WordCount { metadata: ModuleMetadata }

#[async_trait::async_trait]
impl SkillModule for WordCount {
    fn get_metadata(&self) -> &ModuleMetadata { &self.metadata }
    fn health_check(&self) -> bool { true }

    fn available_methods(&self) -> Vec<MethodInfo> {
        vec![MethodInfo {
            method: "count".to_string(),
            description: "Count the words in a piece of text".to_string(),
            args_description: "The text to count".to_string(),
        }]
    }

    async fn execute(&self, method: &str, args: &str) -> Result<SkillOutput, SkillError> {
        match method {
            "count" => Ok(SkillOutput::Text(args.split_whitespace().count().to_string())),
            other => Err(SkillError::NotFound(format!("unknown method '{}'", other))),
        }
    }
}

#[tokio::main]
async fn main() {
    jumabek_sdk::runtime::run_skill(WordCount { /* ... */ }).await.unwrap();
}
```

Build it, drop the binary in `~/.jumabek/skills`, and it is there next start.

### Settings and keys

A skill runs with a stripped environment. It cannot see the agent's own credentials, and it
must never contain a hard-coded key. Whatever you put under `[skills.<name>]` reaches that
skill, and only that skill:

```toml
# config.toml                    # secrets.toml
[skills.weather]                 [skills.weather]
city = "Almaty"                  api_key = "..."
```

```
JUMABEK_SKILL_CITY=Almaty        JUMABEK_SKILL_API_KEY=...
```

---

## Commands

```bash
jumabek                          # start a session
jumabek "how many files here?"   # run one task and exit
jumabek --voice                  # speak instead of typing

jumabek doctor                   # check the setup
jumabek mic                      # watch the microphone level for ten seconds
jumabek where                    # print every path it uses

jumabek skills                   # list installed skills
jumabek remove <name>            # remove one

jumabek jobs                     # list background jobs
jumabek job-stop <id>            # stop and delete one

jumabek backups                  # list snapshots
jumabek restore <id>             # roll back to one
```

Inside a session, `/voice` and `/cli` switch modes without losing the conversation, and
`/quit` leaves. Shift+Enter starts a new line without submitting — Alt+Enter does the same
on terminals that do not report modifier keys.

Answers are rendered, not printed: headings, lists, tables, code blocks and emphasis all
arrive as terminal formatting rather than raw asterisks. Your turn and the agent's are
told apart by a chip against a solid left bar, so a long session stays readable.

### When voice does not hear you

A microphone that goes unheard used to be a silent failure with nothing to look at.
`jumabek mic` opens the device and shows the level against the threshold it has to beat:

```console
       0 |                              | needs     50   quiet
      39 |                              | needs     50   VOICE
     141 |#                             | needs     50   VOICE
      93 |#                             | needs     50   VOICE

  loudest frame: 146
  noise floor settled at: 19
  complete utterances: 1

  The microphone works and speech is being detected.

  The signal is quiet: 146 at its loudest, where speech usually reaches
  a few thousand. It clears the threshold, but transcription will be better
  with the input level raised in the system sound settings.
```

The threshold falls over the first second as the noise floor settles, so a quiet room ends
up more sensitive than a loud one. A sentence has to clear the line for half a second to
count, and finishes after nine hundred milliseconds of silence — which is why the check
waits for you to stop talking rather than cutting at the clock.

Voice mode says the same things as it goes: when it starts listening, how long an utterance
was, and when it heard something it could not make out.

---

## Safety

Self-improvement means running code that did not exist a minute ago. Five things stand
between that and your machine, and each exists because of something that actually went wrong.

**Dangerous commands are stopped by the core, not by the model.** Recursive deletes, disk
formatting, shutdown, a download piped into a shell — all need your word, whether or not the
model thought to ask. Relying on the model to volunteer is not a control: told to skip the
confirmation, it skips it.

**New code is exercised in a container first.** Compiled, then run with no network, a
read-only filesystem, capped CPU and memory, and every capability dropped. Code that hangs,
crashes or reaches for the network is caught there rather than on your disk.

**Every install is preceded by a snapshot.** Rolling back removes a skill that did not exist
at that point, rather than merely restoring the files that did. The rollback itself is
snapshotted first.

**Skills cannot leak processes.** Each runs inside a group killed as a unit, so a shell
command it started does not outlive it — even if the agent itself is killed.

**A background job's rights are fixed before it runs.** Everything else here asks at the
moment it matters. A job cannot: there is nobody at the prompt at three in the morning. So
approving one means approving a list of skills, and separately whether it may write new
skills or step past a safety rule — and the question leads with that list rather than with
the task. A job that tries anything else is refused and says so in its report; it cannot
ask, and it cannot delegate its way around the limit, because a sub-agent inherits the
grant that spawned it.

---

## What it does not do

> [!WARNING]
> **The container is a check, not a jail.** It catches broken and misbehaving code before
> installation. It does not protect against a malicious build script in a dependency,
> because the binary that finally gets installed is compiled natively afterwards. That is
> why the config section is called `preflight` and not `sandbox`.

**Only one LLM router has been tested.** Any OpenAI-compatible endpoint should work. Nobody
has verified that.

**Voice is only half proven.** Capture has now met real hardware: the device is found, the
stream arrives, and speech is detected against the noise floor on a USB headset — that much
is measured, not assumed. What has not been exercised end to end is the rest of the round
trip, transcription through to a spoken answer. The race that made older assistants listen
to their own voice is fixed and measured. The detection thresholds are still tuned to one
room and one microphone; `jumabek mic` will tell you how yours compares.

**Parallelism helps across skills, not within one.** Two calls to the same skill share one
connection and one working directory, so they are deliberately serialised.

---

## License

MIT. Juma — Friday in Kazakh; the one that came after Jarvis.
