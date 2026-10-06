//! Exercise real TCP queries against generic HTTP controller and Kubernetes fixtures.
use std::{collections::HashMap, convert::Infallible, sync::Arc, time::Duration};

use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, body::Bytes, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{Mutex, watch},
    task::JoinHandle,
};

use crate::{
    config::{CapacityDemandConfig, Config},
    k8s_client::K8sClient,
    proxy::{DataProxy, DefaultEndpointCacheHandle},
    query_server::{QueryRequest, QueryServer},
    session::SessionManager,
};

struct State {
    pod: Option<Value>,
    demands: usize,
    watches: usize,
    patches: usize,
    status: StatusCode,
    conflict: bool,
    block_demand: bool,
    ready_on_demand: bool,
}

struct Fixture {
    state: Arc<Mutex<State>>,
    changed: watch::Sender<u64>,
    http: JoinHandle<()>,
    server: QueryServer,
    sessions: SessionManager,
    config: Config,
    k8s: K8sClient,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.http.abort();
    }
}

impl Fixture {
    async fn new(pod: Option<Value>, timeout: u64) -> Self {
        Self::configured(pod, timeout, |_| {}).await
    }

    async fn configured(
        pod: Option<Value>,
        timeout: u64,
        configure: impl FnOnce(&mut Config),
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            pod,
            demands: 0,
            watches: 0,
            patches: 0,
            status: StatusCode::ACCEPTED,
            conflict: false,
            block_demand: false,
            ready_on_demand: false,
        }));
        let (changed, _) = watch::channel(1);
        let http_state = state.clone();
        let http_changed = changed.clone();
        let http = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = http_state.clone();
                let changed = http_changed.clone();
                tokio::spawn(async move {
                    let _ = http1::Builder::new()
                        .serve_connection(
                            TokioIo::new(stream),
                            service_fn(move |request| {
                                handle(request, state.clone(), changed.clone())
                            }),
                        )
                        .await;
                });
            }
        });
        let mut config: Config =
            serde_yaml::from_str(include_str!("../config.example.yaml")).unwrap();
        config.capacity_demand = Some(CapacityDemandConfig {
            endpoint: format!("http://{address}/v1alpha1/demand"),
            backend_groups: HashMap::from([("tutorial".into(), "example-backend".into())]),
            boot_timeout_seconds: timeout,
        });
        configure(&mut config);
        let client = kube::Client::try_from(kube::Config::new(
            format!("http://{address}").parse().unwrap(),
        ))
        .unwrap();
        let sessions = SessionManager::new(300);
        let k8s = K8sClient::from_client(client);
        let server = QueryServer::new(0, k8s.clone(), sessions.clone(), config.clone());
        Self {
            state,
            changed,
            http,
            server,
            sessions,
            config,
            k8s,
        }
    }

    async fn set_pod(&self, mut pod: Value) {
        let mut state = self.state.lock().await;
        self.changed.send_modify(|v| *v += 1);
        pod["metadata"]["resourceVersion"] = json!(self.changed.borrow().to_string());
        state.pod = Some(pod);
    }

    async fn wait_for(&self, predicate: impl Fn(&State) -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut updates = self.changed.subscribe();
            loop {
                if predicate(&*self.state.lock().await) {
                    return;
                }
                updates.changed().await.unwrap();
            }
        })
        .await
        .expect("fixture event timed out");
    }

    async fn query(&self, request: QueryRequest) -> (TcpStream, JoinHandle<anyhow::Result<()>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let server = self.server.clone();
        let task = tokio::spawn(async move { server.handle_connection(stream).await });
        client
            .write_all(&serde_json::to_vec(&request).unwrap())
            .await
            .unwrap();
        (client, task)
    }
}

async fn handle(
    request: Request<hyper::body::Incoming>,
    state: Arc<Mutex<State>>,
    changed: watch::Sender<u64>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let body = request.into_body().collect().await.unwrap().to_bytes();
    let (status, value) = if uri.path() == "/v1alpha1/demand" {
        assert_eq!(method, hyper::Method::POST);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"backendGroup":"example-backend"})
        );
        let mut state = state.lock().await;
        state.demands += 1;
        if state.ready_on_demand {
            changed.send_modify(|v| *v += 1);
            let mut ready = pod(true, false);
            ready["metadata"]["resourceVersion"] = json!(changed.borrow().to_string());
            state.pod = Some(ready);
        }
        changed.send_modify(|_| {});
        if state.block_demand {
            drop(state);
            std::future::pending::<()>().await;
            unreachable!();
        }
        (state.status, json!({}))
    } else if method == hyper::Method::PATCH {
        let patch: Value = serde_json::from_slice(&body).unwrap();
        let mut state = state.lock().await;
        state.patches += 1;
        let pod = state.pod.as_ref().unwrap();
        if state.conflict
            || patch["metadata"]["resourceVersion"] != pod["metadata"]["resourceVersion"]
        {
            (
                StatusCode::CONFLICT,
                json!({"kind":"Status", "apiVersion":"v1", "status":"Failure", "message":"Pod changed", "reason":"Conflict", "code":409}),
            )
        } else {
            let mut pod = pod.clone();
            for (key, value) in patch["metadata"]["labels"].as_object().unwrap() {
                pod["metadata"]["labels"][key] = value.clone();
            }
            changed.send_modify(|v| *v += 1);
            pod["metadata"]["resourceVersion"] = json!(changed.borrow().to_string());
            state.pod = Some(pod.clone());
            (StatusCode::OK, pod)
        }
    } else if uri.query().is_some_and(|q| q.contains("watch=true")) {
        let version = uri
            .query()
            .unwrap()
            .split('&')
            .find_map(|part| part.strip_prefix("resourceVersion="))
            .unwrap()
            .to_owned();
        let mut updates = changed.subscribe();
        {
            state.lock().await.watches += 1;
            changed.send_modify(|_| {});
        }
        let pod = loop {
            {
                let state = state.lock().await;
                if let Some(pod) = &state.pod {
                    if pod["metadata"]["resourceVersion"].as_str().unwrap() != version {
                        break pod.clone();
                    }
                }
            }
            updates.changed().await.unwrap();
        };
        (StatusCode::OK, json!({"type":"MODIFIED", "object":pod}))
    } else {
        let state = state.lock().await;
        let items: Vec<Value> = state.pod.iter().cloned().collect();
        (
            StatusCode::OK,
            json!({"apiVersion":"v1", "kind":"PodList", "metadata":{"resourceVersion":changed.borrow().to_string()}, "items":items}),
        )
    };
    Ok(Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(format!("{value}\n"))))
        .unwrap())
}

fn pod(ready: bool, draining: bool) -> Value {
    json!({"apiVersion":"v1", "kind":"Pod", "metadata":{"name":"example-server", "namespace":"game-servers", "uid":"opaque-example", "resourceVersion":"1",
        "labels":{"app":"game-server", "map":"tutorial", "udp-director.io/draining":draining.to_string()}},
        "spec":{"containers":[{"name":"server", "ports":[{"name":"game", "containerPort":7777}]}]},
        "status":{"phase":"Running", "podIP":"127.0.0.1", "conditions":[{"type":"Ready", "status":if ready {"True"} else {"False"}}]}})
}

fn access(map: &str, character: &str) -> QueryRequest {
    QueryRequest::Query {
        map: map.into(),
        character_id: character.into(),
        friend_ids: vec![],
    }
}

async fn response(mut client: TcpStream, task: JoinHandle<anyhow::Result<()>>) -> Value {
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), client.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    task.await.unwrap().unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn hot_access_assigns_without_signaling() {
    let fixture = Fixture::new(Some(pod(true, false)), 5).await;
    let (client, task) = fixture.query(access("tutorial", "joining")).await;
    assert_eq!(response(client, task).await["status"], "Allocated");
    let state = fixture.state.lock().await;
    assert_eq!(state.demands, 0);
    assert_eq!(state.patches, 1);
    assert_eq!(
        state.pod.as_ref().unwrap()["metadata"]["labels"]["characters.udp-director.io/joining"],
        "Allocated"
    );
    assert_eq!(fixture.sessions.count(), 1);
}

#[tokio::test]
async fn concurrent_cold_queries_signal_once_wait_for_ready_and_assign() {
    let fixture = Fixture::new(None, 5).await;
    let first = fixture.query(access("tutorial", "first")).await;
    let second = fixture.query(access("tutorial", "second")).await;
    fixture.wait_for(|s| s.watches == 2).await;
    assert_eq!(fixture.state.lock().await.demands, 1);
    fixture.set_pod(pod(false, false)).await;
    fixture.wait_for(|s| s.watches >= 4).await;
    assert_eq!(fixture.state.lock().await.patches, 0);
    fixture.set_pod(pod(true, false)).await;
    let results = tokio::join!(response(first.0, first.1), response(second.0, second.1));
    // Resource-version conflicts fail safely; a successful query has a real label.
    assert!(results.0["status"] == "Allocated" || results.1["status"] == "Allocated");
    for result in [results.0, results.1] {
        assert!(result["status"] == "Allocated" || result["error"] == "Unable to establish route");
    }
    assert_eq!(fixture.state.lock().await.demands, 1);
}

#[tokio::test]
async fn startup_timeout_leaves_no_assignment_or_route() {
    let fixture = Fixture::new(None, 1).await;
    let query = fixture.query(access("tutorial", "joining")).await;
    assert_eq!(
        response(query.0, query.1).await["error"],
        "Server startup timed out"
    );
    assert_eq!(fixture.state.lock().await.patches, 0);
    assert_eq!(fixture.sessions.count(), 0);
}

#[tokio::test]
async fn disconnected_cold_query_is_cancelled_and_later_demand_can_signal() {
    let fixture = Fixture::new(None, 5).await;
    let (client, task) = fixture.query(access("tutorial", "leaving")).await;
    fixture.wait_for(|s| s.watches == 1).await;
    drop(client);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let next = fixture.query(access("tutorial", "next")).await;
    fixture.wait_for(|s| s.demands == 2).await;
    fixture.set_pod(pod(true, false)).await;
    assert_eq!(response(next.0, next.1).await["status"], "Allocated");
    assert_eq!(fixture.state.lock().await.patches, 1);
}

#[tokio::test]
async fn draining_pod_excluded_until_marker_clears() {
    let fixture = Fixture::new(Some(pod(true, true)), 5).await;
    let query = fixture.query(access("tutorial", "joining")).await;
    fixture.wait_for(|s| s.watches == 1).await;
    assert_eq!(fixture.state.lock().await.patches, 0);
    fixture.set_pod(pod(true, false)).await;
    assert_eq!(response(query.0, query.1).await["status"], "Allocated");
}

#[tokio::test]
async fn concurrent_drain_patch_conflict_never_installs_route() {
    let fixture = Fixture::new(Some(pod(true, false)), 5).await;
    fixture.state.lock().await.conflict = true;
    let query = fixture.query(access("tutorial", "joining")).await;
    assert_eq!(
        response(query.0, query.1).await["error"],
        "Unable to establish route"
    );
    assert_eq!(fixture.sessions.count(), 0);
}

#[tokio::test]
async fn unknown_cold_map_and_character_lookup_do_not_signal() {
    let fixture = Fixture::new(None, 5).await;
    let query = fixture.query(access("unpublished", "joining")).await;
    assert_eq!(
        response(query.0, query.1).await["error"],
        "Unknown backend group"
    );
    let query = fixture
        .query(QueryRequest::CharacterList {
            map: "tutorial".into(),
            character_ids: vec![],
        })
        .await;
    assert_eq!(response(query.0, query.1).await, json!({"servers":[]}));
    assert_eq!(fixture.state.lock().await.demands, 0);
}

#[tokio::test]
async fn controller_rejections_return_specific_errors_without_assignment() {
    for (status, error) in [
        (StatusCode::NOT_FOUND, "Unknown backend group"),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Backend group is under maintenance",
        ),
        (StatusCode::OK, "Capacity demand unavailable"),
    ] {
        let fixture = Fixture::new(None, 5).await;
        fixture.state.lock().await.status = status;
        let query = fixture.query(access("tutorial", "joining")).await;
        assert_eq!(response(query.0, query.1).await["error"], error);
        let state = fixture.state.lock().await;
        assert_eq!(state.demands, 1);
        assert_eq!(state.patches, 0);
        assert_eq!(state.watches, 0);
    }
}

#[tokio::test]
async fn cancelling_one_cold_waiter_preserves_other_waiter_and_single_signal() {
    let fixture = Fixture::new(None, 5).await;
    let (leaving, cancelled) = fixture.query(access("tutorial", "leaving")).await;
    let remaining = fixture.query(access("tutorial", "remaining")).await;
    fixture.wait_for(|s| s.watches == 2).await;
    drop(leaving);
    cancelled.await.unwrap().unwrap();
    fixture.set_pod(pod(true, false)).await;
    assert_eq!(
        response(remaining.0, remaining.1).await["status"],
        "Allocated"
    );
    let state = fixture.state.lock().await;
    assert_eq!(state.demands, 1);
    assert_eq!(state.patches, 1);
    assert!(
        state.pod.as_ref().unwrap()["metadata"]["labels"]
            .get("characters.udp-director.io/leaving")
            .is_none()
    );
}

#[tokio::test]
async fn cold_start_tcp_assignment_then_plain_udp_round_trip() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let backend = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let gameplay = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let director_addr = gameplay.local_addr().unwrap();
        let fixture = Fixture::configured(None, 5, |config| {
            config.data_port = Some(director_addr.port())
        })
        .await;
        let query = fixture.query(access("tutorial", "joining")).await;
        fixture.wait_for(|s| s.watches == 1).await;
        let mut ready = pod(true, false);
        ready["spec"]["containers"][0]["ports"][0]["containerPort"] =
            json!(backend.local_addr().unwrap().port());
        fixture.set_pod(ready).await;
        let reply = response(query.0, query.1).await;
        assert_eq!(reply["status"], "Allocated");
        assert_eq!(reply["ports"]["default"], director_addr.port());
        let proxy = DataProxy::new(
            fixture.sessions.clone(),
            fixture.config.clone(),
            fixture.k8s.clone(),
            DefaultEndpointCacheHandle::new(),
        );
        let forwarding =
            tokio::spawn(async move { proxy.run_udp_socket(gameplay, director_addr.port()).await });
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        udp.send_to(b"ordinary-gameplay", director_addr)
            .await
            .unwrap();
        let mut buffer = [0; 128];
        let (size, upstream) = backend.recv_from(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..size], b"ordinary-gameplay");
        backend.send_to(b"server-reply", upstream).await.unwrap();
        let (size, source) = udp.recv_from(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..size], b"server-reply");
        assert_eq!(source, director_addr);
        forwarding.abort();
        fixture.sessions.clear_all().await;
    })
    .await
    .expect("cold startup and UDP forwarding timed out");
}

#[tokio::test]
async fn readiness_change_during_callback_is_not_missed() {
    let fixture = Fixture::new(None, 5).await;
    fixture.state.lock().await.ready_on_demand = true;
    let query = fixture.query(access("tutorial", "joining")).await;
    assert_eq!(response(query.0, query.1).await["status"], "Allocated");
    assert_eq!(fixture.state.lock().await.demands, 1);
}

#[tokio::test]
async fn startup_deadline_also_bounds_a_stalled_callback() {
    let fixture = Fixture::new(None, 1).await;
    fixture.state.lock().await.block_demand = true;
    let query = fixture.query(access("tutorial", "joining")).await;
    assert_eq!(
        response(query.0, query.1).await["error"],
        "Server startup timed out"
    );
    assert_eq!(fixture.sessions.count(), 0);
}
