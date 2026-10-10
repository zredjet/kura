use super::*;
use crate::jobs::{EventSink, JobFilter, JobKind, JobManager, JobState};
use crate::library::Library;
use crate::playback::support::SupportEntry;

fn os(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

fn streams(video: Option<&str>, audio: Option<&str>) -> Streams {
    Streams {
        video: video.map(str::to_string),
        audio: audio.map(str::to_string),
        video_index: video.map(|_| 0),
        audio_index: audio.map(|_| 1),
        width: Some(1920),
        height: Some(1080),
        duration_ms: Some(10_000),
    }
}

fn joined(plan: ProxyPlan, s: &Streams) -> String {
    os(&proxy_args(
        Path::new("/l/in.mkv"),
        Path::new("/l/out.mp4.part"),
        plan,
        s,
        VIDEO_ENCODER,
    ))
    .join(" ")
}

#[test]
fn builds_ffmpeg_arguments() {
    use StreamAction::{Copy, Encode, None};
    let remux = joined(
        ProxyPlan::Make {
            video: Copy,
            audio: Copy,
        },
        &streams(Some("h264"), Some("aac")),
    );
    assert_eq!(
        remux,
        "-hide_banner -v error -nostdin -y -i /l/in.mkv -map 0:0 -map 0:1 -c:v copy -c:a copy \
         -sn -dn -strict experimental -movflags +faststart -f mp4 -progress pipe:1 -nostats /l/out.mp4.part"
    );
    // Safari 向けに HEVC は hvc1 の印を付ける
    let hevc = joined(
        ProxyPlan::Make {
            video: Copy,
            audio: Encode,
        },
        &streams(Some("hevc"), Some("vorbis")),
    );
    assert!(
        hevc.contains("-c:v copy -tag:v hvc1 -c:a aac -b:a 256k"),
        "{hevc}"
    );
    let encode = joined(
        ProxyPlan::Make {
            video: Encode,
            audio: Copy,
        },
        &streams(Some("mpeg4"), Some("mp3")),
    );
    assert!(
        encode.contains("-c:v h264_videotoolbox -b:v 10M -pix_fmt yuv420p -allow_sw 1 -c:a copy"),
        "{encode}"
    );
    let audio_only = joined(
        ProxyPlan::Make {
            video: None,
            audio: Encode,
        },
        &streams(Option::None, Some("vorbis")),
    );
    assert!(
        audio_only.contains("-map 0:1 -vn -c:a aac -b:a 256k"),
        "{audio_only}"
    );
    assert!(!audio_only.contains("0:0"), "{audio_only}");
    assert!(
        proxy_args(
            Path::new("a"),
            Path::new("b"),
            ProxyPlan::NotNeeded,
            &streams(Some("h264"), Option::None),
            VIDEO_ENCODER
        )
        .is_empty()
    );
}

fn insert(conn: &Connection, id: i64, kind: &str, ext: &str, state: &str, imported_at: i64) {
    conn.execute(
        "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at, proxy_state)
         VALUES (?1, ?2, ?3, ?4, 1, 't', 't', ?5, ?5, ?6)",
        params![id, vec![id as u8; 32], ext, kind, imported_at, state],
    )
    .unwrap();
}

fn ids(work: &Work) -> Vec<i64> {
    match work {
        Work::Decide(batch) => batch.iter().map(|t| t.id).collect(),
        Work::Make(todo) => vec![-todo.id],
        Work::Nothing => vec![],
    }
}

#[test]
fn picks_prioritized_work_first() {
    let conn = crate::db::test_conn();
    insert(&conn, 1, "video", "mkv", "unknown", 10);
    insert(&conn, 2, "video", "mkv", "pending", 20);
    insert(&conn, 3, "audio", "ogg", "unknown", 30);
    insert(&conn, 4, "audio", "ogg", "pending", 40);
    insert(&conn, 5, "video", "mp4", "not_needed", 50);
    insert(&conn, 6, "image", "jpg", "not_needed", 60);

    // 優先なし：新しい順に判定 → 新しい順に作成
    assert_eq!(ids(&next_work(&conn, &[]).unwrap()), vec![3, 1]);
    // 優先：判定待ち → 作成待ち（作成は負の数で表す）
    assert_eq!(ids(&next_work(&conn, &[2, 1]).unwrap()), vec![1]);
    assert_eq!(ids(&next_work(&conn, &[5, 2, 4]).unwrap()), vec![-2]);

    conn.execute(
        "UPDATE items SET proxy_state = 'not_needed' WHERE id IN (1, 3)",
        [],
    )
    .unwrap();
    assert_eq!(ids(&next_work(&conn, &[]).unwrap()), vec![-4]);
    // ゴミ箱のものは扱わない
    conn.execute("UPDATE items SET trashed_at = 1 WHERE id IN (2, 4)", [])
        .unwrap();
    assert!(matches!(next_work(&conn, &[2]).unwrap(), Work::Nothing));
    assert!(!has_todo(&conn).unwrap());
}

#[test]
fn todo_lookups_use_indexes() {
    let conn = crate::db::test_conn();
    for state in ["unknown", "pending"] {
        let plan = crate::db::query_plan(
            &conn,
            &format!(
                "SELECT id, hash, ext, kind FROM items
                 WHERE proxy_state = '{state}' AND kind != 'image' AND trashed_at IS NULL
                 ORDER BY imported_at DESC, id DESC LIMIT 16"
            ),
        );
        assert!(plan.contains(&format!("idx_items_proxy_{state}")), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }
}

#[test]
fn changed_support_resets_decisions_but_keeps_ready_proxies() {
    let conn = crate::db::test_conn();
    insert(&conn, 1, "video", "mkv", "ready", 10);
    insert(&conn, 2, "video", "mp4", "not_needed", 20);
    insert(&conn, 3, "audio", "ogg", "failed", 30);
    insert(&conn, 4, "image", "jpg", "not_needed", 40);
    let state = |id: i64| -> String {
        conn.query_row("SELECT proxy_state FROM items WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    };

    assert!(sync_support(&conn, "aaaa").unwrap());
    assert_eq!(
        (state(1), state(2), state(3), state(4)),
        (
            "ready".into(),
            "unknown".into(),
            "unknown".into(),
            "not_needed".into()
        )
    );
    // 同じ表ならやり直さない
    conn.execute(
        "UPDATE items SET proxy_state = 'not_needed' WHERE id IN (2, 3)",
        [],
    )
    .unwrap();
    assert!(!sync_support(&conn, "aaaa").unwrap());
    assert_eq!(state(2), "not_needed");
    assert!(sync_support(&conn, "bbbb").unwrap());
    assert_eq!(state(2), "unknown");

    // 再生できなかったものは作成待ちに戻す（作成済み・画像はそのまま）
    conn.execute(
        "UPDATE items SET proxy_state = 'not_needed' WHERE id IN (2, 3)",
        [],
    )
    .unwrap();
    assert!(force_pending(&conn, &[1, 2, 4]).unwrap());
    assert_eq!(
        (state(1), state(2), state(4)),
        ("ready".into(), "pending".into(), "not_needed".into())
    );
    assert!(!any_todo(&conn, &[1, 3, 4]).unwrap());
}

/// 実際の ffmpeg で、mkv（h264 / aac）の入れ替えと ogg（vorbis）の変換を確かめる。ffmpeg がなければ飛ばす。
#[test]
fn real_ffmpeg_makes_proxies_when_available() {
    let Some(tools) = ProxyTools::detect() else {
        eprintln!("ffmpeg がないので飛ばします");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
    let fixtures: [(i64, &str, &str, &[&str]); 3] = [
        (
            1,
            "video",
            "mkv",
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=25:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ],
        ),
        (
            2,
            "audio",
            "ogg",
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=660:duration=2",
                "-c:a",
                "vorbis",
                "-strict",
                "-2",
                "-ac",
                "2",
            ],
        ),
        (
            3,
            "video",
            "mp4",
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=25:duration=1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ],
        ),
    ];
    for (id, kind, ext, args) in fixtures {
        let hash = [id as u8; 32];
        let path = lib.layout().content_path(&hash, ext);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let status = Command::new(&tools.ffmpeg)
            .args(["-v", "error", "-y"])
            .args(args)
            .arg(&path)
            .status()
            .unwrap();
        if !status.success() {
            eprintln!("テスト用のファイルを作れないので飛ばします（{ext}）");
            return;
        }
        let (kind, ext) = (kind.to_string(), ext.to_string());
        lib.db()
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 1, 't', 't', ?1, ?1)",
                    params![id, hash.to_vec(), ext, kind],
                )?;
                Ok(())
            })
            .unwrap();
    }
    let rows: &[(Container, &str)] = &[
        (Container::Mp4, "h264"),
        (Container::Mp4, "aac"),
        (Container::Ogg, "opus"),
    ];
    let support = PlaybackSupport::from_entries(
        &rows
            .iter()
            .map(|(container, codec)| SupportEntry {
                container: *container,
                codec: (*codec).to_string(),
                playable: true,
            })
            .collect::<Vec<_>>(),
    );
    let shared = Arc::new(ProxyShared::default());
    shared.set_support(support);
    let sink: EventSink = Arc::new(|_| {});
    let jobs = JobManager::new(
        lib.clone(),
        vec![(
            JobKind::Proxy,
            Arc::new(ProxyHandler::new(Some(tools.clone()), shared)),
        )],
        sink,
    )
    .unwrap();
    jobs.ensure_queued(JobKind::Proxy, "{}").unwrap();
    jobs.start().unwrap();
    assert!(jobs.wait_idle(Duration::from_secs(60)));
    let last = jobs
        .list(&JobFilter {
            kinds: vec![JobKind::Proxy],
            states: vec![],
            limit: Some(1),
        })
        .unwrap();
    assert_eq!(last[0].state, JobState::Done, "{:?}", last[0]);

    let state = |id: i64| -> (String, Option<String>) {
        lib.db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT proxy_state, codec_info FROM items WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap()
    };
    assert_eq!(state(1).0, "ready");
    assert_eq!(state(2).0, "ready");
    assert_eq!(state(3).0, "not_needed");
    assert_eq!(
        state(1).1.as_deref(),
        Some(r#"{"audio":"aac","codec":"h264","video":"h264"}"#)
    );
    // 作ったプロキシは mp4 の h264 / aac
    for (id, video, audio) in [(1u8, Some("h264"), Some("aac")), (2, None, Some("aac"))] {
        let proxy = lib.layout().proxy_path(&hex(&[id; 32]));
        assert!(proxy.is_file());
        assert!(!proxy.with_extension("mp4.part").exists());
        let streams = tools.probe(&proxy).unwrap();
        assert_eq!(
            (streams.video.as_deref(), streams.audio.as_deref()),
            (video, audio)
        );
    }
}

/// 実際の ffmpeg で、読めない映像（avi の mpeg4）を VideoToolbox で変換する。エンコーダがなければ飛ばす。
#[test]
fn real_videotoolbox_encodes_when_available() {
    let Some(tools) = ProxyTools::detect() else {
        eprintln!("ffmpeg がないので飛ばします");
        return;
    };
    let encoders = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output()
        .unwrap();
    if !String::from_utf8_lossy(&encoders.stdout).contains(VIDEO_ENCODER) {
        eprintln!("{VIDEO_ENCODER} がないので飛ばします");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("in.avi");
    let status = Command::new(&tools.ffmpeg)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=640x360:rate=25:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-c:v",
            "mpeg4",
            "-c:a",
            "pcm_s16le",
            "-shortest",
        ])
        .arg(&src)
        .status()
        .unwrap();
    assert!(status.success());
    let streams = tools.probe(&src).unwrap();
    let plan = plan(Container::Other, &streams, &PlaybackSupport::default());
    let out = dir.path().join("out.mp4.part");
    let args = proxy_args(&src, &out, plan, &streams, VIDEO_ENCODER);
    let progress = Mutex::new(Vec::new());
    let result = run_ffmpeg(
        &tools,
        &args,
        streams.duration_ms,
        &|| Control::Continue,
        &|f| progress.lock().unwrap().push(f),
    );
    assert_eq!(result, Ok(None));
    let made = tools.probe(&out).unwrap();
    assert_eq!(
        (made.video.as_deref(), made.audio.as_deref()),
        (Some("h264"), Some("aac"))
    );
    let progress = progress.into_inner().unwrap();
    assert!(progress.last().is_some_and(|f| *f > 0.5), "{progress:?}");

    // 中止を指示すれば止まる
    let stopped = run_ffmpeg(
        &tools,
        &args,
        streams.duration_ms,
        &|| Control::Cancel,
        &|_| {},
    );
    assert_eq!(stopped, Ok(Some(Control::Cancel)));
}
