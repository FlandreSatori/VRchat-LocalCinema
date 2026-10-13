//! HTTP 文件服务集成测试：Range / HEAD / 路径遍历 / 端口冲突 / 流式读取

use std::io::Write;
use std::sync::{Arc, Mutex};

use app_lib::http_server::HttpServer;
use app_lib::media_library::Db;
use app_lib::settings::{Settings, UrlMode};
use app_lib::url_builder::{build_media_url, UrlInput};

fn setup() -> (HttpServer, app_lib::media_library::MediaItem, PathBufWrap, String) {
    let dir = std::env::temp_dir().join(format!("vlc-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();

    // 生成 1 MiB 测试文件
    let video = dir.join("测试 视频 #1.mp4");
    let mut f = std::fs::File::create(&video).unwrap();
    let chunk: Vec<u8> = (0..65536).map(|i| (i % 251) as u8).collect();
    for _ in 0..16 {
        f.write_all(&chunk).unwrap();
    }
    drop(f);
    let size = std::fs::metadata(&video).unwrap().len();

    let db = Db::open(&dir.join("library.db")).unwrap();
    let (item, _) = db.register(&video).unwrap();
    assert_eq!(item.file_size, size);
    let id = item.id.clone();

    let thumb_dir = dir.join("thumbnails");
    std::fs::create_dir_all(&thumb_dir).unwrap();

    let server = HttpServer::new(Arc::new(Mutex::new(db)), &thumb_dir);
    (server, item, PathBufWrap(dir), id)
}

struct PathBufWrap(std::path::PathBuf);
impl Drop for PathBufWrap {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn wait_running(server: &HttpServer) -> u16 {
    for _ in 0..50 {
        let s = server.status();
        if s.running {
            return s.port;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("server did not start");
}

#[tokio::test]
async fn test_http_media_serving() {
    let (server, item, _dir, id) = setup();
    server.start("127.0.0.1", 0).await.expect("start failed");
    let port = wait_running(&server).await;
    let base = format!("http://127.0.0.1:{}", port);
    let client = reqwest::Client::new();

    // 1. GET 完整文件
    let url = build_media_url(&item, &UrlInput {
        mode: UrlMode::Localhost, port, lan_ip: None,
    }).unwrap();
    assert!(url.is_ascii());
    assert_eq!(client.get(format!("{}/media/{}", base, id)).send().await.unwrap().status(), 200);
    assert_eq!(client.get(format!("{}/media/{}/legacy.mp4", base, id)).send().await.unwrap().status(), 200);
    assert_eq!(client.get(format!("{}/", base)).send().await.unwrap().text().await.unwrap(), "VRchat-LocalCinema");
    for attack in ["..%2Flibrary.db", "..%5Clibrary.db", "%2Fetc%2Fpasswd", "D%3A%5Cvideo.mp4", "library.db", "not-registered.mp4"] {
        assert_eq!(client.get(format!("{}/{}", base, attack)).send().await.unwrap().status(), 404, "{}", attack);
    }
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("accept-ranges").unwrap().to_str().unwrap(),
        "bytes"
    );
    let clen: u64 = resp
        .headers()
        .get("content-length")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(clen, 65536 * 16);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len() as u64, 65536 * 16);
    assert_eq!(&body[0..4], &[0u8, 1, 2, 3]);

    // 2. HEAD
    let resp = client.head(&url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-length").unwrap().to_str().unwrap(),
        (65536 * 16).to_string()
    );
    assert!(resp.bytes().await.unwrap().is_empty());

    // 3. Range 有效请求 → 206
    let resp = client
        .get(&url)
        .header("Range", "bytes=100-199")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 206);
    assert_eq!(
        resp.headers().get("content-range").unwrap().to_str().unwrap(),
        format!("bytes 100-199/{}", 65536 * 16)
    );
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), 100);
    assert_eq!(body[0], 100u8);

    // 4. Range 越界 → 416
    let resp = client
        .get(&url)
        .header("Range", "bytes=99999999999-")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 416);
    assert_eq!(
        resp.headers().get("content-range").unwrap().to_str().unwrap(),
        format!("bytes */{}", 65536 * 16)
    );

    // 5. Range 后缀请求 bytes=-500
    let resp = client
        .get(&url)
        .header("Range", "bytes=-500")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 206);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), 500);

    // 6. 未知媒体 id → 404
    let resp = client
        .get(format!("{}/media/{}/x.mp4", base, uuid::Uuid::new_v4()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // 7. 路径遍历攻击（thumb 路由）→ 404
    let resp = client
        .get(format!("{}/thumb/..%2F..%2F..%2Flibrary.db", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    // 反斜杠遍历
    let resp = client
        .get(format!("{}/thumb/..%5C..%5Clibrary.db", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // 8. 并发访问
    let mut handles = Vec::new();
    for _ in 0..8 {
        let c = client.clone();
        let u = url.clone();
        handles.push(tokio::spawn(async move {
            c.get(&u)
                .header("Range", "bytes=0-1023")
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .len()
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap(), 1024);
    }

    // 9. 读取期间删除文件不应 panic
    let url2 = url.clone();
    let c2 = client.clone();
    let (server2, item2, _dir2, _id2) = setup();
    server2.start("127.0.0.1", 0).await.unwrap();
    let port2 = wait_running(&server2).await;
    let url_del = format!("http://127.0.0.1:{}/media/{}/a.mp4", port2, item2.id);
    let req = c2.get(&url_del).header("Range", "bytes=0-1048575").send();
    let _ = std::fs::remove_file(&item2.file_path); // 删除源文件
    let resp = req.await.unwrap();
    // 已经打开的文件句柄在 Windows 上仍可读到部分内容或直接成功/失败，但服务不能崩溃
    assert!(resp.status().as_u16() == 200 || resp.status().as_u16() == 206 || resp.status().as_u16() == 404 || resp.status().as_u16() == 500);
    drop(url2);

    // 10. 端口冲突 → 明确报错
    let (server3, _item3, _dir3, _id3) = setup();
    let err = server3.start("127.0.0.1", port).await.unwrap_err();
    assert!(err.to_string().contains("端口"), "错误消息应指出端口问题: {}", err);

    server.stop();
    server2.stop();
    server3.stop();
}

#[test]
fn test_url_builder_encoding() {
    let dir = std::env::temp_dir().join(format!("vlc-test-url-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let video = dir.join("中文 文件&名#1.mp4");
    std::fs::write(&video, b"dummy").unwrap();
    let db = Db::open(&dir.join("library.db")).unwrap();
    let (item, _) = db.register(&video).unwrap();

    let input = UrlInput {
        mode: UrlMode::Localhost,
        port: 17001,
        lan_ip: None,
    };
    let url = build_media_url(&item, &input).unwrap();
    assert_eq!(url, "http://127.0.0.1:17001/%E4%B8%AD%E6%96%87%20%E6%96%87%E4%BB%B6%26%E5%90%8D%231.mp4");
    assert!(url.is_ascii());
    assert_eq!(app_lib::url_builder::encode_segment("test video 测试#?%.mp4"), "test%20video%20%E6%B5%8B%E8%AF%95%23%3F%25.mp4");
    // 特殊字符必须被编码
    assert!(!url.contains('#'));
    assert!(!url.contains(' '));
    assert!(url.contains("%23"));

    let input_lan = UrlInput {
        mode: UrlMode::Lan,
        port: 17001,
        lan_ip: Some("192.168.1.10".to_string()),
    };
    let url_lan = build_media_url(&item, &input_lan).unwrap();
    assert!(url_lan.starts_with("http://192.168.1.10:17001/"));

    let input_bad = UrlInput {
        mode: UrlMode::Lan,
        port: 17001,
        lan_ip: None,
    };
    assert!(build_media_url(&item, &input_bad).unwrap_err().to_string().contains("未检测到"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_batch_import_conflicts() {
    let dir = PathBufWrap(std::env::temp_dir().join(format!("vlc-conflict-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(dir.0.join("a")).unwrap();
    std::fs::create_dir_all(dir.0.join("b")).unwrap();
    let first = dir.0.join("a/测试 Video.mp4");
    let conflict = dir.0.join("b/测试 video.MP4");
    let other = dir.0.join("b/other%.mp4");
    for path in [&first, &conflict, &other] { std::fs::write(path, b"video").unwrap(); }
    let db = Db::open(&dir.0.join("library.db")).unwrap();
    let paths: Vec<_> = [&first, &conflict, &other, &first].into_iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let result = db.import_paths(&paths);
    assert_eq!(result.items.len(), 3);
    assert_eq!(result.items[0].id, result.items[2].id);
    assert_eq!(result.skipped.len(), 1);
    assert_eq!(result.skipped[0].path, paths[1]);
    assert!(result.skipped[0].reason.contains("同名文件冲突"));
    assert_eq!(db.list().unwrap().len(), 2);
    assert_eq!(std::fs::read(&conflict).unwrap(), b"video");
    assert!(serde_json::to_value(&result).unwrap()["skipped"][0]["reason"].as_str().unwrap().contains("同名"));
}

#[tokio::test]
async fn test_existing_duplicate_names_are_ambiguous() {
    let dir = PathBufWrap(std::env::temp_dir().join(format!("vlc-legacy-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let file = dir.0.join("same.mp4");
    std::fs::write(&file, b"video").unwrap();
    let db_path = dir.0.join("library.db");
    let db = Db::open(&db_path).unwrap();
    let (item, _) = db.register(&file).unwrap();
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute("INSERT INTO media SELECT 'legacy', title, 'SAME.MP4', 'legacy-path', file_size, duration_ms, format, thumbnail, status, added_at, updated_at FROM media WHERE id = ?1", [&item.id]).unwrap();
    drop(conn);
    assert!(db.resolve_basename(&item.file_name).unwrap_err().to_string().contains("请先移除冲突条目"));
    assert_eq!(db.register(&file).unwrap().0.id, item.id);
    let server = HttpServer::new(Arc::new(Mutex::new(db)), &dir.0);
    server.start("127.0.0.1", 0).await.unwrap();
    let base = format!("http://127.0.0.1:{}", server.status().port);
    let client = reqwest::Client::new();
    let response = client.get(format!("{}/same.mp4", base)).send().await.unwrap();
    assert_eq!(response.status(), 409);
    assert!(response.text().await.unwrap().contains("移除冲突"));
    assert_eq!(client.get(format!("{}/media/{}", base, item.id)).send().await.unwrap().status(), 200);
    server.stop();
}

#[test]
fn test_settings_persistence() {
    let dir = std::env::temp_dir().join(format!("vlc-test-set-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("settings.json");

    assert_eq!(Settings::default().port, 17001);
    assert_eq!(Settings::load(&file).unwrap().port, 17001);
    std::fs::write(&file, "{}").unwrap();
    assert_eq!(Settings::load(&file).unwrap().port, 17001);
    std::fs::write(&file, r#"{"port":8080}"#).unwrap();
    assert_eq!(Settings::load(&file).unwrap().port, 8080);
    let mut s = Settings::default();
    s.port = 9999;
    s.easytier_ip = "10.0.0.1".to_string();
    s.save(&file).unwrap();

    let loaded = Settings::load(&file).unwrap();
    assert_eq!(loaded.port, 9999);
    assert_eq!(loaded.easytier_ip, "10.0.0.1");

    std::fs::write(&file, r#"{"listenMode":"custom","defaultUrlMode":"easytier","customIp":"10.0.0.1","port":23456,"defaultPlayerPath":"player.exe","defaultPlayerArgs":"{path}"}"#).unwrap();
    let migrated = Settings::load(&file).unwrap();
    assert_eq!(migrated.bind_ip(), "0.0.0.0");
    assert_eq!(migrated.default_url_mode, UrlMode::Lan);
    assert_eq!(migrated.port, 23456);
    assert_eq!(migrated.default_player_path, "player.exe");
    assert_eq!(migrated.default_player_args, "{path}");
    std::fs::write(&file, r#"{"listenMode":"loopback","defaultUrlMode":"lan"}"#).unwrap();
    let local = Settings::load(&file).unwrap();
    assert_eq!(local.bind_ip(), "127.0.0.1");
    assert_eq!(local.default_url_mode, UrlMode::Localhost);

    // 损坏的配置应报错而不是 panic（由上层回退到默认）
    std::fs::write(&file, "{ not json").unwrap();
    assert!(Settings::load(&file).is_err());

    let _ = std::fs::remove_dir_all(&dir);
}
