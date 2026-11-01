//! Unix-domain clients against real local listeners; no external service is required.
#![cfg(unix)]

use pi_jsrt::{JsRuntime, RuntimeOptions};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

async fn run(path: &std::path::Path, source: &str) -> serde_json::Value {
    let rt = JsRuntime::new(RuntimeOptions::default()).unwrap();
    let source = source.replace("SOCKET_PATH", &serde_json::to_string(path).unwrap());
    let ns = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        rt.eval_module(&source, "unix-client.mjs"),
    )
    .await
    .expect("Unix client timed out")
    .unwrap_or_else(|e| panic!("{e}\n{:?}", e.as_js().and_then(|j| j.stack.clone())));
    ns.get("default").await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_client_connect_forms_and_fragmented_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..4 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap(),
                json!({"state": "working"})
            );
            let reply = "{\"ok\":\"🦀\"}\n".as_bytes();
            stream.get_mut().write_all(&reply[..9]).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            stream.get_mut().write_all(&reply[9..]).await.unwrap();
            stream.get_mut().shutdown().await.unwrap();
            let mut remaining = Vec::new();
            stream.read_to_end(&mut remaining).await.unwrap();
            assert!(remaining.is_empty());
        }
    });
    let result = run(&path, r#"
        import net from 'node:net';
        const path = SOCKET_PATH;
        const results = [];
        for (let form = 0; form < 4; form++) {
            const events = [];
            const callback = () => events.push('callback');
            const socket = form === 0 ? net.connect(path, callback)
                : form === 1 ? net.createConnection({ path, port: Symbol('ignored'), host: 'ignored.invalid', autoSelectFamily: Symbol('ignored') }, callback)
                : form === 2 ? new net.Socket().connect(path, callback)
                : new net.Socket().connect({ path }, callback);
            const chunks = [];
            socket.on('connect', () => events.push('connect'));
            socket.on('ready', () => events.push('ready'));
            socket.on('data', data => chunks.push(data));
            socket.on('end', () => events.push('end'));
            const closed = new Promise((resolve, reject) => {
                socket.on('error', reject);
                socket.on('close', hadError => { events.push('close'); resolve(hadError); });
            });
            // Queue writes before the asynchronous connection completes.
            socket.write('{"state":');
            socket.write('"working"}\n');
            const hadError = await closed;
            results.push({ events, hadError, reply: Buffer.concat(chunks).toString('utf8'),
                address: socket.address(), remote: socket.remoteAddress ?? null });
        }
        export default results;
    "#).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let expected = json!({"events": ["callback", "connect", "ready", "end", "close"],
        "hadError": false, "reply": "{\"ok\":\"🦀\"}\n", "address": {}, "remote": null});
    assert_eq!(result, json!([expected, expected, expected, expected]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_client_connection_errors_include_path() {
    let dir = tempfile::tempdir().unwrap();
    for (name, code) in [("missing.sock", "ENOENT"), ("stale.sock", "ECONNREFUSED")] {
        let path = dir.path().join(name);
        if code == "ECONNREFUSED" {
            drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        }
        let result = run(
            &path,
            r#"
            import net from 'node:net';
            const path = SOCKET_PATH;
            const socket = net.connect(path);
            const events = [];
            let error;
            socket.on('connect', () => events.push('connect'));
            socket.on('error', e => {
                events.push('error');
                error = { code: e.code, errno: e.errno, syscall: e.syscall, address: e.address,
                    port: e.port, message: e.message };
            });
            const hadError = await new Promise(resolve => socket.on('close', resolve));
            export default { events, error, hadError };
        "#,
        )
        .await;
        assert_eq!(
            result,
            json!({"events": ["error"], "hadError": true,
            "error": {"code": code,
                "errno": -if code == "ENOENT" { libc::ENOENT } else { libc::ECONNREFUSED },
                "syscall": "connect", "address": path,
                "message": format!("connect {code} {}", path.display())}})
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_client_invalid_arguments_match_node() {
    let result = run(
        std::path::Path::new("unused"),
        r#"
        import net from 'node:net';
        const results = [];
        for (const path of [null, undefined, 42, true, {}]) {
            try {
                net.connect({ path });
                results.push('unexpected success');
            } catch (e) {
                results.push({ name: e.name, code: e.code, message: e.message });
            }
        }
        export default results;
    "#,
    )
    .await;
    let missing = json!({"name": "TypeError", "code": "ERR_MISSING_ARGS",
        "message": "The \"options\" or \"port\" or \"path\" argument must be specified"});
    let invalid = |received: &str| {
        json!({"name": "TypeError", "code": "ERR_INVALID_ARG_TYPE",
        "message": format!("The \"options.path\" property must be of type string. Received {received}")})
    };
    assert_eq!(
        result,
        json!([
            missing,
            missing,
            invalid("type number (42)"),
            invalid("type boolean (true)"),
            invalid("an instance of Object")
        ])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_client_rejects_embedded_nul_asynchronously() {
    let result = run(
        std::path::Path::new("bad\0suffix"),
        r#"
        import net from 'node:net';
        const path = SOCKET_PATH;
        const events = [];
        const socket = net.connect(path);
        events.push('returned');
        socket.on('connect', () => events.push('connect'));
        let error;
        socket.on('error', e => {
            events.push('error');
            error = { code: e.code, errno: e.errno, syscall: e.syscall,
                address: e.address, message: e.message };
        });
        const hadError = await new Promise(resolve => socket.on('close', resolve));
        export default { events, error, hadError };
    "#,
    )
    .await;
    assert_eq!(
        result,
        json!({"events": ["returned", "error"], "hadError": true,
        "error": {"code": "EINVAL", "errno": -libc::EINVAL, "syscall": "connect",
            "address": "bad\0suffix", "message": "connect EINVAL bad\0suffix - Local (undefined:undefined)"}})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn falsy_unix_paths_use_the_tcp_port() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        for _ in 0..5 {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"tcp").await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    let source = r#"
        import net from 'node:net';
        const replies = [];
        for (const path of [null, '', false, 0, undefined]) {
            const socket = net.connect({ path, port: TCP_PORT, host: '127.0.0.1' });
            let reply = '';
            socket.on('data', data => reply += data.toString());
            await new Promise((resolve, reject) => {
                socket.on('error', reject);
                socket.on('close', resolve);
            });
            replies.push(reply);
        }
        export default replies;
    "#
    .replace("TCP_PORT", &port.to_string());
    assert_eq!(
        run(std::path::Path::new("unused"), &source).await,
        json!(["tcp", "tcp", "tcp", "tcp", "tcp"])
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_client_accepts_abstract_names_with_nuls() {
    use std::os::linux::net::SocketAddrExt;
    let dir = tempfile::tempdir().unwrap();
    let path = format!(
        "\0pi-jsrt-{}\0suffix",
        dir.path().file_name().unwrap().to_str().unwrap()
    );
    let address =
        std::os::unix::net::SocketAddr::from_abstract_name(&path.as_bytes()[1..]).unwrap();
    let listener = std::os::unix::net::UnixListener::bind_addr(&address).unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener = UnixListener::from_std(listener).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.write_all(b"abstract").await.unwrap();
        stream.shutdown().await.unwrap();
    });
    let result = run(
        std::path::Path::new(&path),
        r#"
        import net from 'node:net';
        const socket = net.connect(SOCKET_PATH);
        let reply = '';
        socket.on('data', data => reply += data.toString());
        await new Promise((resolve, reject) => {
            socket.on('error', reject);
            socket.on('close', resolve);
        });
        export default reply;
    "#,
    )
    .await;
    assert_eq!(result, json!("abstract"));
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_client_timeout_and_destroy_close_the_peer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timeout.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut data = Vec::new();
        stream.read_to_end(&mut data).await.unwrap();
        assert_eq!(data, [0, 255, 13, 10]);
    });
    let result = run(
        &path,
        r#"
        import net from 'node:net';
        const socket = net.connect(SOCKET_PATH);
        socket.on('error', e => { throw e; });
        let timedOut = false;
        socket.on('connect', () => {
            socket.write(Buffer.from([0, 255, 13, 10]));
            socket.setTimeout(25, () => { timedOut = true; socket.destroy(); });
        });
        const hadError = await new Promise(resolve => socket.on('close', resolve));
        export default { timedOut, hadError, destroyed: socket.destroyed };
    "#,
    )
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        result,
        json!({"timedOut": true, "hadError": false, "destroyed": true})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_client_destroy_while_connecting_never_emits_connect() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancel.sock");
    let _listener = UnixListener::bind(&path).unwrap();
    let result = run(
        &path,
        r#"
        import net from 'node:net';
        const events = [];
        for (let i = 0; i < 32; i++) {
            const socket = net.connect(SOCKET_PATH);
            socket.on('connect', () => events.push('connect'));
            socket.on('error', e => events.push(e.code));
            const closed = new Promise(resolve => socket.on('close', resolve));
            socket.destroy();
            events.push(await closed);
        }
        await new Promise(resolve => setTimeout(resolve, 20));
        export default events;
    "#,
    )
    .await;
    assert_eq!(result, json!(vec![false; 32]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_client_runtime_shutdown_closes_connected_socket() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shutdown.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    });
    assert_eq!(
        run(
            &path,
            r#"
        import net from 'node:net';
        const socket = net.connect(SOCKET_PATH);
        await new Promise((resolve, reject) => {
            socket.on('connect', resolve);
            socket.on('error', reject);
        });
        export default true;
    "#
        )
        .await,
        json!(true)
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
