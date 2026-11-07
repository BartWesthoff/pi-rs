//! TCP error contracts using loopback port zero; no external DNS or service.
use pi_jsrt::{JsRuntime, RuntimeOptions};
use serde_json::json;

async fn run(source: &str) -> serde_json::Value {
    let rt = JsRuntime::new(RuntimeOptions::default()).unwrap();
    let ns = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        rt.eval_module(source, "tcp-errors.mjs"),
    )
    .await
    .expect("TCP error fixture timed out")
    .unwrap();
    ns.get("default").await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_zero_errors_omit_port_and_are_asynchronous() {
    let result = run(r#"
        import net from 'node:net';
        const results = [];
        for (const options of [{port: 0, host: '127.0.0.1'}, {path: '', host: '127.0.0.1'}]) {
            const events = [];
            const socket = net.connect(options);
            events.push('returned');
            let error;
            socket.on('error', e => {
                events.push('error');
                error = {name: e.name, code: e.code, errno: e.errno, message: e.message,
                    syscall: e.syscall, address: e.address, port: e.port};
            });
            const hadError = await new Promise(resolve => socket.on('close', resolve));
            results.push({events, error, hadError});
        }
        export default results;
    "#)
    .await;
    let expected = json!({"events": ["returned", "error"], "hadError": true,
        "error": {"name": "Error", "code": "ECONNREFUSED", "errno": -libc::ECONNREFUSED,
            "message": "connect ECONNREFUSED 127.0.0.1", "syscall": "connect", "address": "127.0.0.1"}});
    assert_eq!(result, json!([expected, expected]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn localhost_failures_preserve_each_address_error() {
    let addresses: Vec<_> = tokio::net::lookup_host(("localhost", 0))
        .await
        .unwrap()
        .map(|a| a.ip().to_string())
        .collect();
    let result = run(r#"
        import net from 'node:net';
        const results = [];
        const original = net.getDefaultAutoSelectFamily();
        for (const mode of ['default', 'false', 'global-false', 'override']) {
            net.setDefaultAutoSelectFamily(mode !== 'global-false' && mode !== 'override');
            const options = {port: 0, host: 'localhost'};
            if (mode === 'false') options.autoSelectFamily = false;
            if (mode === 'override') options.autoSelectFamily = true;
            const socket = net.connect(options);
            let error;
            socket.on('error', e => {
                const fields = e => ({name: e.name, code: e.code, errno: e.errno,
                    message: e.message, syscall: e.syscall, address: e.address, port: e.port});
                error = {...fields(e), aggregate: e instanceof AggregateError,
                    errors: e.errors?.map(fields)};
            });
            const hadError = await new Promise(resolve => socket.on('close', resolve));
            results.push({mode, error, hadError});
        }
        net.setDefaultAutoSelectFamily(original);
        export default {original, results};
    "#)
    .await;
    assert_eq!(result["original"], true);
    let mut unique = Vec::new();
    for address in addresses {
        if !unique.contains(&address) {
            unique.push(address);
        }
    }
    assert!(!unique.is_empty());
    for row in result["results"].as_array().unwrap() {
        assert_eq!(row["hadError"], true);
        // Node's ADDRCONFIG can filter out loopback-only IPv6 from localhost.
        let multiple = row["error"]["aggregate"].as_bool().unwrap();
        if multiple {
            assert!(row["mode"] == "default" || row["mode"] == "override");
            assert!(unique.len() > 1);
        }
        let errors = if multiple {
            assert_eq!(row["error"]["name"], "AggregateError");
            assert_eq!(row["error"]["message"], "");
            assert!(row["error"].get("errno").is_none());
            let errors = row["error"]["errors"].as_array().unwrap().clone();
            assert!(errors.len() > 1 && errors.len() <= unique.len());
            assert_eq!(row["error"]["code"], errors[0]["code"]);
            errors
        } else {
            assert_eq!(row["error"]["name"], "Error");
            vec![row["error"].clone()]
        };
        for error in errors {
            assert!(unique.contains(&error["address"].as_str().unwrap().to_string()));
            assert!(error["errno"].as_i64().unwrap() < 0);
            assert_eq!(error["syscall"], "connect");
            assert!(error.get("port").is_none());
            assert_eq!(
                error["message"],
                format!(
                    "connect {} {}",
                    error["code"].as_str().unwrap(),
                    error["address"].as_str().unwrap()
                )
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aggregate_error_event_preserves_child_errors_without_dual_stack() {
    let result = run(r#"
        import net from 'node:net';
        const socket = new net.Socket();
        const children = ['::1', '127.0.0.1'].map(address => ({
            message: 'connect ECONNREFUSED ' + address, code: 'ECONNREFUSED',
            errno: -111, syscall: 'connect', address
        }));
        let result;
        socket.on('error', e => {
            result = {name: e.name, message: e.message, code: e.code,
                aggregate: e instanceof AggregateError,
                children: e.errors.map(child => ({name: child.name, ...child,
                    message: child.message}))};
        });
        const closed = new Promise(resolve => socket.on('close', resolve));
        socket._onNetEvent('error', {message: '', code: 'ECONNREFUSED', errors: children});
        result = await closed && result;
        export default result;
    "#)
    .await;
    assert_eq!(
        result,
        json!({"name": "AggregateError", "message": "", "code": "ECONNREFUSED", "aggregate": true,
        "children": [
            {"name": "Error", "message": "connect ECONNREFUSED ::1", "code": "ECONNREFUSED", "errno": -111, "syscall": "connect", "address": "::1"},
            {"name": "Error", "message": "connect ECONNREFUSED 127.0.0.1", "code": "ECONNREFUSED", "errno": -111, "syscall": "connect", "address": "127.0.0.1"}
        ]})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn family_selection_validates_boolean_inputs() {
    let result = run(r#"
        import net from 'node:net';
        const errors = [];
        for (const call of [() => net.setDefaultAutoSelectFamily(42),
            () => net.connect({port: 0, autoSelectFamily: 42})]) {
            try { call(); errors.push('unexpected success'); }
            catch (e) { errors.push({code: e.code, message: e.message}); }
        }
        export default errors;
    "#)
    .await;
    assert_eq!(
        result,
        json!([
            {"code": "ERR_INVALID_ARG_TYPE", "message": "The \"value\" argument must be of type boolean. Received type number (42)"},
            {"code": "ERR_INVALID_ARG_TYPE", "message": "The \"options.autoSelectFamily\" property must be of type boolean. Received type number (42)"}
        ])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostname_connect_falls_back_to_a_listening_address() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.write_all(b"fallback").await.unwrap();
        stream.shutdown().await.unwrap();
    });
    let source = r#"
        import net from 'node:net';
        const socket = net.connect({host: 'localhost', port: TCP_PORT});
        let reply = '';
        socket.on('data', data => reply += data.toString());
        await new Promise((resolve, reject) => {
            socket.on('error', reject);
            socket.on('close', resolve);
        });
        export default reply;
    "#
    .replace("TCP_PORT", &port.to_string());
    assert_eq!(run(&source).await, json!("fallback"));
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_destroy_while_resolving_never_emits_connect_or_error() {
    let result = run(r#"
        import net from 'node:net';
        const events = [];
        for (let i = 0; i < 16; i++) {
            const socket = net.connect({host: 'localhost', port: 0});
            socket.on('connect', () => events.push('connect'));
            socket.on('error', e => events.push(e.code));
            const closed = new Promise(resolve => socket.on('close', resolve));
            socket.destroy();
            events.push(await closed);
        }
        await new Promise(resolve => setTimeout(resolve, 25));
        export default events;
    "#)
    .await;
    assert_eq!(result, json!(vec![false; 16]));
}
