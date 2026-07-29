# JumaBek

A personal assistant that runs on your own machine, does real work on it, and writes itself
new abilities when the ones it has are not enough.

Its skills are ordinary programs. JumaBek starts them, talks to them over a small JSON
protocol, and can compile new ones on the spot — each one checked inside a container before
it is allowed anywhere near your files.

---

## What it looks like

Asked for something it has no skill for, JumaBek notices, explains what is missing, and asks
before writing anything:

```
> мне нужно регулярно парсить RSS-ленты и доставать заголовки

  У меня есть shell_executor, но он умеет только запускать команды — для парсинга
  RSS нужен HTTP-клиент и XML-парсер, которых в оболочке нет. Предлагаю создать
  модуль rss_parser. Давай я начну?

  permission  MEDIUM   write a new skill 'rss_parser'
  Write, compile and install a new skill 'rss_parser'. The code is written by the
  model and compiled on this machine; once installed it loads in every future session.

  allow? [y/N] y
  allowed

  · rss_parser: preflight passed in docker engine 28.5.1 —
      build: 2 cpu / 2g ram, network on · run: 0.5 cpu / 256m ram, network none, read-only
  · rss_parser: built and validated
  · rss_parser is live: fetch_titles, fetch_titles_formatted

  · rss_parser · fetch_titles_formatted

  Вот заголовки с Hacker News, которые я получил: ...
```

Forty-nine seconds from the request to a working result. The new skill stays installed and
loads on every later run.

---

## Install

Prebuilt archives for Windows, Linux and macOS are on the [releases page](../../releases).

```
# Windows
irm https://raw.githubusercontent.com/japo0nn/jumabek/main/install.ps1 | iex

# Linux, macOS
curl -fsSL https://raw.githubusercontent.com/japo0nn/jumabek/main/install.sh | bash
```

The installer puts everything under `~/.jumabek`, adds it to your PATH, and never overwrites
a config you have already edited. It offers to install a local LLM router but does not do it
behind your back.

From source:

```
cargo install --git https://github.com/japo0nn/jumabek
```

Then set a key and start:

```
export JUMABEK_API_KEY="your-key"     # or put it in ~/.jumabek/secrets.toml
jumabek
```

---

## What it needs

| | Without it |
|---|---|
| An OpenAI-compatible LLM endpoint | nothing works |
| Rust toolchain | JumaBek runs, but cannot write itself new skills |
| Docker | new skills are refused, because they cannot be checked first |
| ffmpeg | voice mode is unavailable; typing still works |

`jumabek doctor` reports all of it, and says what each gap costs you:

```
  ok   home         C:\Users\sosa\.jumabek
  ok   config       C:\Users\sosa\.jumabek\config.toml
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

### About the LLM endpoint

JumaBek was developed and tested against
[OmniRoute](https://www.npmjs.com/package/omniroute), a local router that puts many
providers behind a single OpenAI-compatible endpoint:

```
npm i -g omniroute
omniroute serve
```

Any other OpenAI-compatible endpoint should work — the client sends `model`, `messages` and
`stream`, and reads `choices[0].message.content` — but nothing else has been tested. Point
`[llm].base_uri` wherever you like.

---

## How it works

Every skill is a separate process. JumaBek writes a line of JSON to its stdin and reads a
line back from its stdout:

```
core  ->  {"id":1,"method":"execute","params":"{\"method\":\"run\",\"args\":\"ls\"}"}
skill ->  {"id":1,"payload":{"Output":{"Text":"file1.txt\nfile2.txt"}}}
```

That is the whole contract. It buys several things at once:

- a skill can be written in any language, not just Rust
- a crashing or hanging skill cannot take the agent down with it
- adding a skill means dropping a binary into `~/.jumabek/skills`, with no rebuild of anything
- the same interface serves a skill that ships with JumaBek and one it wrote five minutes ago

Skills start lazily. A session with twenty installed skills starts twenty times faster than
it would if each had to introduce itself, because their descriptions are cached and the
binaries only run when something calls them.

### Memory

Everything said is stored in SQLite. The current session is always in context; older
sessions are searched only when the model asks for them, through a full-text index with
Russian and English stemming, so `файл` finds `файлами`.

When a conversation outgrows the context window, the oldest exchanges are dropped in whole
task groups — never half of one — and replaced by a marker telling the model what it can
still recall on request.

---

## Writing skills

You can write one yourself, or let JumaBek do it. Either way it is one file:

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
must never contain a hard-coded key.

Whatever you put under `[skills.<name>]` reaches that skill, and only that skill, as
environment variables:

```toml
# config.toml
[skills.weather]
city = "Almaty"        ->  JUMABEK_SKILL_CITY

# secrets.toml
[skills.weather]
api_key = "..."        ->  JUMABEK_SKILL_API_KEY
```

---

## Commands

```
jumabek                          start a session
jumabek "how many files here?"   run one task and exit
jumabek --mode voice             speak instead of typing

jumabek doctor                   check the setup
jumabek where                    print every path it uses

jumabek skills                   list installed skills
jumabek remove <name>            remove one

jumabek backups                  list snapshots
jumabek restore <id>             roll back to one
```

Inside a session, `/voice` and `/cli` switch modes without losing the conversation, and
`/quit` leaves.

---

## Safety

Self-improvement means the agent runs code that did not exist a minute ago. Four things
stand between that and your machine.

**Dangerous commands are stopped by the core, not by the model.** Recursive deletes, disk
formatting, shutdown, piping a download into a shell — these require your confirmation
whether or not the model thought to ask. Relying on the model to volunteer is not a
control: told to skip the confirmation, it skips it.

**New skills are exercised in a container first.** They are compiled and then run with no
network, a read-only filesystem, capped CPU and memory, and every capability dropped. Code
that hangs, crashes or reaches for the network is caught there rather than on your disk.

**Every install is preceded by a snapshot.** `jumabek backups` lists them, `jumabek restore`
puts things back — including removing a skill that did not exist at that point.

**Skills cannot outlive themselves.** Each runs inside a process group that is killed as a
unit, so a shell command it started does not survive it. A skill that stops answering is
killed and restarted on the next call rather than hanging the agent.

---

## Honest limitations

**The container is a check, not a jail.** It catches broken and misbehaving code before
installation. It does not protect against a malicious build script in a dependency, because
the binary that actually gets installed is built natively afterwards. That is why the config
section is called `preflight` and not `sandbox`.

**Only OmniRoute has been tested.** Other OpenAI-compatible endpoints should work. Nobody
has verified that.

**Voice has not been tested on real hardware.** The logic is covered by tests over synthetic
audio, but the detection thresholds are educated guesses and will likely need tuning for
your microphone and room.

**Parallel execution helps across skills, not within one.** Two calls to the same skill share
one connection and one working directory, so they are deliberately serialised.

---

## License

MIT
