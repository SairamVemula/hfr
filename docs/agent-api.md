# Agent API

`hfr-agent` is a small HTTP/1.1 JSON service. It binds to `127.0.0.1:7878` by default and is meant to be reached through an SSH tunnel. Base URL below is `http://127.0.0.1:7878`. The current API version is **2** (`GET /version`); clients refuse to talk to an older agent and tell the user to run `hfr setup`.

## Model

```
Project  -- a folder of files on the VPS (the Hyperframes project)
  `- Job -- one run of `npx hyperframes render ...` in that project, with its own options and log
       `- OutputFile -- a video the job wrote
```

- A project holds any number of jobs, so you can render the same files again with other options without uploading them again.
- **One running job per project.** Starting another gets `409`.
- Output files are attributed to the job that wrote them (videos with a modification time inside the job's start..finish window). They are recorded when the job ends and can be listed, downloaded and deleted one by one.
- Names are labels only. Two projects with the same name are two projects (they have different ids).

On disk (`HFR_ROOT`, `/var/lib/hfr/jobs` when installed by `hfr setup`):

```
<project-id>/project.json        metadata and the manifest of synced files
<project-id>/project/            the files
<project-id>/jobs/<job-id>/job.json, log.txt
```

An agent that finds the layout of the first version (`<id>/{project,log.txt,meta.json}`) migrates each folder into a project with one job at start-up. Nothing is deleted.

## Authentication

Every request needs `Authorization: Bearer <HFR_TOKEN>`. A wrong or missing token gets `401 Unauthorized` after a 400 ms delay. The token is compared in constant time.

## Errors

Plain-text bodies with these statuses: `400` bad input (disallowed command, unsafe path or archive), `401` auth, `404` unknown project, job or file, `409` wrong state (a job is already running in the project, nothing synced yet, a sync is in progress, deleting while running), `416` bad byte range, `500` internal.

## Types

### Project
```json
{ "id": "a9037190-...", "name": "promo-video", "created_unix": 1792998661, "size_bytes": 2097152050, "state": "ready" }
```
`state`: `empty` (no files yet), `syncing`, `ready`. `size_bytes` includes render outputs.

### Job
```json
{
  "id": "5fd9f710-...", "project_id": "a9037190-...",
  "state": "done",
  "command": ["npx", "hyperframes", "render", "--fps", "60"],
  "created_unix": 1792998802, "started_unix": 1792998802, "finished_unix": 1792998842,
  "files": [ { "path": "renders/intro.mp4", "name": "intro.mp4", "size": 29238, "created_unix": 1792998840, "sha256": "ab12..." } ]
}
```
`state`: `running`, `done`, `failed`, `cancelled`. `files[].sha256` is `null` for a moment after the job ends (hashing a big file takes time). The options of a job are the ones in `command`.

### Metrics
```json
{ "cpu_total": 2.6, "cpu_cores": [1.2, 0.4], "mem_used": 673218560, "mem_total": 8287846400,
  "swap_used": 0, "swap_total": 0, "load1": 0.07, "disk_used": 40265318400, "disk_total": 206910000000 }
```
Percentages are 0-100; sizes are bytes. `disk_*` describes the largest filesystem.

## Endpoints

| Method and path | Purpose | Success |
| --- | --- | --- |
| `GET /version` | `{ "api": 2, "agent": "0.1.2" }` | `200` |
| `GET /status` | `{ "running_jobs": n, "projects": n }` (used by the installer to see whether the VPS is busy) | `200` |
| `GET /overview` | Every project with its jobs in one call: `[{ "project": Project, "jobs": [Job] }]`, newest first | `200` |
| `GET /metrics` | Server metrics (samples CPU over ~200 ms) | `200` Metrics |
| `GET /projects` | List projects | `200` `[Project]` |
| `POST /projects` | Create an empty project. Body `{ "name": "..." }` | `200` Project |
| `GET /projects/{p}` | One project | `200` |
| `DELETE /projects/{p}` | Delete the project, its files, jobs and outputs. `409` while a job is running | `204` |
| `GET /projects/{p}/manifest` | Files that came from syncs: `[{ "path", "size", "sha256" }]` | `200` |
| `POST /projects/{p}/sync/plan` | Body: the client's manifest. Answer: `{ "upload": [paths], "delete": [paths] }`. Only files that came from an earlier sync can be in `delete`; render outputs never are | `200` |
| `PUT /projects/{p}/sync` | Upload **only the files in `upload`** as a raw **tar** body (streamed, no size limit). They are extracted over the project and the manifest is updated. `409` while a job runs | `204` |
| `POST /projects/{p}/sync/delete` | Body `{ "paths": [...] }`: remove files that no longer exist locally | `204` |
| `POST /projects/{p}/jobs` | Create **and start** a job. Body `{ "command": ["npx","hyperframes","render", ...] }` (allowlist below). `409` if a job is running or the project has no files | `200` Job |
| `GET /projects/{p}/jobs` | Jobs of the project, newest first | `200` `[Job]` |
| `GET /projects/{p}/jobs/{j}` | One job | `200` Job |
| `POST /projects/{p}/jobs/{j}/cancel` | Stop a running render and its whole process tree (SIGTERM to the process group, SIGKILL after 5 s). The job becomes `cancelled`. `409` if it is not running | `202` |
| `DELETE /projects/{p}/jobs/{j}` | Remove the job, its log and its recorded output files. The project's own files stay. `409` while running | `204` |
| `GET /projects/{p}/jobs/{j}/logs` | **Server-Sent Events**: one `data: <line>` event per output line, from the moment you connect | `200` stream |
| `GET /projects/{p}/jobs/{j}/log` | The whole log so far as text (works after the job ended) | `200` text |
| `GET /projects/{p}/jobs/{j}/files` | Output files of the job | `200` `[OutputFile]` |
| `GET /projects/{p}/jobs/{j}/file?path=...` | Download one output file. Supports `Range: bytes=N-` (`206`, `Content-Range`, `Accept-Ranges: bytes`) so an interrupted download can resume | `200` / `206` bytes |
| `DELETE /projects/{p}/jobs/{j}/file?path=...` | Delete one recorded output file. Any other path is `404`; this can never remove a source file | `204` |

## Command allowlist

`POST /projects/{p}/jobs` accepts only `npx hyperframes render` (rules in `crates/proto/src/command.rs`) followed by recognised options (list in [cli.md](cli.md)) and optionally `.`. Values for `-o/--output`, `-c/--composition` and `--variables-file` must be relative paths without `..`, `:` or a leading slash. Commands are executed directly (no shell). Setting `HFR_ALLOW_ANY_COMMAND=1` disables this check and exists only for development.

## Archive rules

Entries must be regular files or directories with relative paths made of normal components. Symlinks, hard links, devices and absolute or `..` paths reject the whole upload with `400`. Permissions are not preserved.

## Example (through a tunnel on port 17878)

```bash
TOKEN=...   # from the credential store / /etc/hfr/agent.env
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:17878/overview
curl -N -H "Authorization: Bearer $TOKEN" http://127.0.0.1:17878/projects/<p>/jobs/<j>/logs
# resume a download after 1,000,000 bytes
curl -H "Authorization: Bearer $TOKEN" -H "Range: bytes=1000000-" -o part2 \
  "http://127.0.0.1:17878/projects/<p>/jobs/<j>/file?path=renders/intro.mp4"
```

`scripts/e2e-agent.sh` exercises every endpoint above against a real agent binary with a fake `npx`.
