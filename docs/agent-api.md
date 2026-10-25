# Agent API

`hfr-agent` is a small HTTP/1.1 JSON service. It binds to `127.0.0.1:7878` by default and is meant to be reached through an SSH tunnel. Base URL below is `http://127.0.0.1:7878`.

## Authentication

Every request needs `Authorization: Bearer <HFR_TOKEN>`. A wrong or missing token gets `401 Unauthorized` after a 400 ms delay. The token is compared in constant time.

## Errors

Plain-text bodies with these statuses: `400` bad input (disallowed command, unsafe archive), `401` auth, `404` unknown job or no output, `409` wrong state (already running, nothing uploaded, still running on delete, job not finished), `500` internal.

## Types

### Job
```json
{
  "id": "14511aaa-0ea9-4afc-99bf-9091a52d198c",
  "name": "my-project",
  "state": "done",
  "command": ["npx", "hyperframes", "render"],
  "created_unix": 1792960606,
  "size_bytes": 12345678
}
```
`state` is one of `created`, `uploaded`, `running`, `done`, `failed`.

### OutputInfo
```json
{ "name": "project_2026-10-25_22-13-15.mp4", "size": 29238, "sha256": "ab12..." }
```

### Metrics
```json
{
  "cpu_total": 2.6, "cpu_cores": [1.2, 0.4],
  "mem_used": 673218560, "mem_total": 8287846400,
  "swap_used": 0, "swap_total": 0,
  "load1": 0.07,
  "disk_used": 40265318400, "disk_total": 206910000000
}
```
Percentages are 0-100; sizes are bytes. `disk_*` describes the largest filesystem.

## Endpoints

| Method and path | Purpose | Success |
| --- | --- | --- |
| `POST /jobs` | Create a job. Body: `{"name": "...", "command": ["npx","hyperframes","render", ...]}` | `200` Job |
| `GET /jobs` | List jobs, newest first | `200` `[Job]` |
| `GET /jobs/{id}` | One job | `200` Job |
| `PUT /jobs/{id}/upload` | Upload the project as a raw **tar** body (streamed, no size limit). Extracted into `project/`; job becomes `uploaded` | `204` |
| `POST /jobs/{id}/start` | Run the job's command in `project/`. `409` if already running or nothing uploaded | `202` |
| `GET /jobs/{id}/logs` | **Server-Sent Events**: one `data: <line>` event per output line, from the moment you connect | `200` stream |
| `GET /jobs/{id}/log` | The whole log so far as text (works after the job ended) | `200` text |
| `GET /jobs/{id}/output/info` | Name, size and SHA-256 of the render output. `409` unless the job is `done`; `404` if no video was written | `200` OutputInfo |
| `GET /jobs/{id}/output` | Stream the output file with `Content-Length` (no `Range` support yet) | `200` bytes |
| `DELETE /jobs/{id}` | Remove the job and its files. `409` while running | `204` |
| `GET /metrics` | Server metrics (samples CPU over ~200 ms) | `200` Metrics |

## Command allowlist

`POST /jobs` accepts only `npx hyperframes render` (rules in `crates/proto/src/command.rs`) followed by recognised options (list in [cli.md](cli.md)) and optionally `.`. Values for `-o/--output`, `-c/--composition` and `--variables-file` must be relative paths without `..`, `:` or a leading slash. Commands are executed directly (no shell). Setting `HFR_ALLOW_ANY_COMMAND=1` disables this check and exists only for development.

## Archive rules

Entries must be regular files or directories with relative paths made of normal components. Symlinks, hard links, devices and absolute or `..` paths reject the whole upload with `400`. Permissions are not preserved.

## Example (through a tunnel on port 17878)

```bash
TOKEN=...   # from the credential store / /etc/hfr/agent.env
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:17878/jobs
curl -N -H "Authorization: Bearer $TOKEN" http://127.0.0.1:17878/jobs/<id>/logs
```
