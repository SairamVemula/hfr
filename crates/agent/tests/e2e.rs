//! End to end: the real agent binary driven by the real client library (sync, renders, files, downloads).

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use hfr_core::{download, sync, Cancelled, Client, Config, Transfer};
use hfr_proto::{JobState, ProjectState};

struct Agent {
    child: Child,
    root: PathBuf,
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hfr-e2e-{name}-{}-{}", std::process::id(), Instant::now().elapsed().as_nanos()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn start_agent() -> (Agent, Client) {
    let port = free_port();
    let root = tmp("agent");
    let child = Command::new(env!("CARGO_BIN_EXE_hfr-agent"))
        .env("HFR_TOKEN", "t")
        .env("HFR_ROOT", &root)
        .env("HFR_ADDR", format!("127.0.0.1:{port}"))
        .env("HFR_ALLOW_ANY_COMMAND", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start agent");
    let agent = Agent { child, root };
    let cfg = Config { token: "t".into(), direct_url: Some(format!("http://127.0.0.1:{port}")), ..Config::default() };
    let client = Client::connect(&cfg).await.unwrap();
    for _ in 0..50 {
        if client.version().await.is_ok() {
            return (agent, client);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("agent did not start");
}

/// A "render" that writes a 300 kB video-like file into renders/.
fn fake_render() -> Vec<String> {
    if cfg!(windows) {
        vec![
            "powershell".into(),
            "-NoProfile".into(),
            "-Command".into(),
            "New-Item -ItemType Directory -Force renders | Out-Null; $b = New-Object byte[] 300000; (New-Object Random 7).NextBytes($b); [IO.File]::WriteAllBytes('renders/out.mp4', $b); 'done'".into(),
        ]
    } else {
        vec!["sh".into(), "-c".into(), "mkdir -p renders && head -c 300000 /dev/urandom > renders/out.mp4 && echo done".into()]
    }
}

async fn wait_done(client: &Client, pid: &str, jid: &str) -> hfr_proto::Job {
    let t0 = Instant::now();
    loop {
        let j = client.job(pid, jid).await.unwrap();
        if j.state != JobState::Running {
            return j;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "render did not finish");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn write(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn projects_sync_renders_files_and_downloads() {
    // Keep the client's own data (hash cache, registry) away from the real config.
    let data = tmp("data");
    std::env::set_var("HFR_CONFIG", data.join("config.toml"));
    let (_agent, client) = start_agent().await;
    client.ensure_compatible().await.unwrap();

    // --- project and delta sync
    let src = tmp("src");
    write(&src, "index.html", "<html>one</html>");
    write(&src, "assets/a.txt", "alpha");
    write(&src, "b.txt", "beta");
    let project = client.create_project("demo").await.unwrap();
    assert_eq!(project.state, ProjectState::Empty);
    assert!(client.create_job(&project.id, fake_render()).await.is_err(), "an empty project cannot render");

    let r1 = sync::sync_project(&client, &project.id, &src, Transfer::new()).await.unwrap();
    assert_eq!((r1.total_files, r1.uploaded_files, r1.deleted_files), (3, 3, 0));
    let r2 = sync::sync_project(&client, &project.id, &src, Transfer::new()).await.unwrap();
    assert_eq!((r2.uploaded_files, r2.uploaded_bytes, r2.deleted_files), (0, 0, 0), "nothing changed: nothing is sent");

    write(&src, "index.html", "<html>two</html>");
    std::fs::remove_file(src.join("b.txt")).unwrap();
    let r3 = sync::sync_project(&client, &project.id, &src, Transfer::new()).await.unwrap();
    assert_eq!((r3.uploaded_files, r3.deleted_files), (1, 1), "only the edited file goes up, the removed one is deleted");
    let overview = client.overview().await.unwrap();
    assert_eq!(overview[0].project.state, ProjectState::Ready);

    // A cancelled sync reports Cancelled.
    let t = Transfer::new();
    t.cancel();
    write(&src, "c.txt", "gamma");
    let err = sync::sync_project(&client, &project.id, &src, t).await.unwrap_err();
    assert!(err.is::<Cancelled>());

    // --- two renders in the same project, each with its own file
    let j1 = client.create_job(&project.id, fake_render()).await.unwrap();
    let j1 = wait_done(&client, &project.id, &j1.id).await;
    assert_eq!(j1.state, JobState::Done, "render failed");
    assert_eq!(j1.files.len(), 1);
    assert_eq!(j1.files[0].path, "renders/out.mp4");
    // Same output name on purpose: the second render replaced the first one's file on the VPS, so the first job must
    // no longer claim it.
    let j2 = client.create_job(&project.id, fake_render()).await.unwrap();
    let j2 = wait_done(&client, &project.id, &j2.id).await;
    assert_eq!(j2.state, JobState::Done);
    assert_eq!(client.overview().await.unwrap()[0].jobs.len(), 2);
    assert!(client.job_files(&project.id, &j1.id).await.unwrap().is_empty(), "the replaced file is no longer listed for the first render");

    // --- downloads: verified, never overwriting, resumable
    tokio::time::sleep(Duration::from_millis(1500)).await; // the agent hashes the file in the background
    let file = client.job_files(&project.id, &j2.id).await.unwrap().remove(0);
    assert_eq!(file.sha256.as_ref().map(|s| s.len()), Some(64), "hash should be known by now");
    let dl_a = tmp("dl-a");
    let t = Transfer::new();
    let d1 = download::download_file(&client, &project.id, &j2.id, &file, &dl_a, t.clone()).await.unwrap();
    assert!(d1.verified);
    assert_eq!(std::fs::metadata(&d1.path).unwrap().len(), file.size);
    assert_eq!(d1.path.file_name().unwrap().to_string_lossy(), "out.mp4");
    let d2 = download::download_file(&client, &project.id, &j2.id, &file, &dl_a, Transfer::new()).await.unwrap();
    assert_eq!(d2.path.file_name().unwrap().to_string_lossy(), "out (1).mp4", "an existing file is never overwritten");

    // Resume: leave the first 100,000 bytes as an unfinished download, then download again.
    let dl_b = tmp("dl-b");
    let plan = download::plan(&dl_b, &project.id, &j2.id, &file).unwrap();
    let whole = std::fs::read(&d1.path).unwrap();
    std::fs::write(&plan.part, &whole[..100_000]).unwrap();
    let t = Transfer::new();
    let d3 = download::download_file(&client, &project.id, &j2.id, &file, &dl_b, t.clone()).await.unwrap();
    assert!(d3.verified, "the resumed file must match the checksum");
    assert_eq!(std::fs::read(&d3.path).unwrap(), whole, "resumed download is byte-identical");
    assert!(!plan.part.exists() && !plan.meta.exists(), "leftovers are cleaned up");

    // A cancelled (paused) download keeps its partial file.
    let dl_c = tmp("dl-c");
    let t = Transfer::new();
    t.cancel();
    let err = download::download_file(&client, &project.id, &j2.id, &file, &dl_c, t).await.unwrap_err();
    assert!(err.is::<Cancelled>());

    // --- delete one file, one render, the project
    client.delete_file(&project.id, &j2.id, &file.path).await.unwrap();
    assert!(client.job_files(&project.id, &j2.id).await.unwrap().is_empty());
    client.delete_job(&project.id, &j1.id).await.unwrap();
    assert_eq!(client.overview().await.unwrap()[0].jobs.len(), 1);
    client.delete_project(&project.id).await.unwrap();
    assert!(client.overview().await.unwrap().is_empty());

    for d in [&src, &dl_a, &dl_b, &dl_c, &data] {
        let _ = std::fs::remove_dir_all(d);
    }
}
