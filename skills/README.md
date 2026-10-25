# Agent skills

Ready-made [Agent Skills](https://docs.claude.com/en/docs/claude-code/skills) so an AI agent can use or work on HFR Remote without reading the whole repo.

| Skill | Use it when |
| --- | --- |
| [`hfr-remote-render`](hfr-remote-render/SKILL.md) | The agent should **operate** the tool: render a Hyperframes project on the user's VPS, follow it, check server health, download, clean up. |
| [`hfr-remote-develop`](hfr-remote-develop/SKILL.md) | The agent should **change** this codebase: architecture, commands, recipes, rules, gotchas. |

## Install

**Claude Code, this repository:** already available. `scripts/sync-skills.sh` mirrors them into `.claude/skills/`, which Claude Code loads automatically.

**Claude Code, any project (user-wide):**

```bash
mkdir -p ~/.claude/skills
cp -r skills/hfr-remote-render ~/.claude/skills/
```

**Other agents:** most skill-aware agents read a `SKILL.md` with `name` / `description` front matter; point yours at these folders, or at [`AGENTS.md`](../AGENTS.md) and [`llms.txt`](../llms.txt) which index the same knowledge. If you use the `skills` CLI: `npx skills add SairamVemula/hfr`.

## Keeping them accurate

Skills are documentation that an agent will trust. When commands, flags or the allowlist change, update the skill in the same pull request, then run `bash scripts/sync-skills.sh`.
