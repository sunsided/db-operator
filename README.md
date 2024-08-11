## db-operator

[![ci](https://github.com/kube-rs/controller-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/kube-rs/controller-rs/actions/workflows/ci.yml)

A Rust Kubernetes operator for PostgreSQL database management, with observability instrumentation.
This operator concerns itself with managing databases and user grants to databases only; it is not aiming
at being the next [SchemaHero](https://schemahero.io/)
or [postgres-operator](https://github.com/CrunchyData/postgres-operator).

The `Controller` object reconciles `DatabaseServer` and `Database` instances when changes to them are detected, and
optionally creates and deletes databases in the referenced servers.

## Installation

### CRD

Apply the CRD from [cached file](yaml/crd.yaml), or pipe it from `crdgen` to pickup schema changes:

```sh
cargo run --bin crdgen | kubectl apply -f -
```

### Controller

Install the controller via `helm` by setting your preferred settings. For defaults:

```sh
helm template charts/db-operator | kubectl apply -f -
kubectl wait --for=condition=available deploy/db-operator --timeout=30s
kubectl port-forward service/db-operator 8080:80
```

### OpenTelemetry

Build and run with `telemetry` feature, or configure it via `helm`:

```sh
helm template charts/db-operator --set tracing.enabled=true | kubectl apply -f -
```

This requires an OpenTelemetry collector in your
cluster. [Tempo](https://github.com/grafana/helm-charts/tree/main/charts/tempo) / [opentelemetry-operator](https://github.com/open-telemetry/opentelemetry-helm-charts/tree/main/charts/opentelemetry-operator) / [grafana agent](https://github.com/grafana/helm-charts/tree/main/charts/agent-operator)
should all work out of the box. If your collector does not support grpc otlp you need to change the exporter in [
`telemetry.rs`](./src/telemetry.rs).

Note that
the [images are pushed either with or without the telemetry feature](https://hub.docker.com/r/sunsided/db-operator/tags/)
depending on whether the tag includes `otel`.

### Metrics

Metrics is available on `/metrics` and a `ServiceMonitor` is configurable from the chart:

```sh
helm template charts/db-operator --set serviceMonitor.enabled=true | kubectl apply -f -
```

## Running

### Locally

```sh
cargo run
```

or, with optional telemetry:

```sh
OPENTELEMETRY_ENDPOINT_URL=https://0.0.0.0:55680 RUST_LOG=info,kube=trace,controller=debug cargo run --features=telemetry
```

### In-cluster

For prebuilt, edit the [chart values](./charts/db-operator/values.yaml) or [snapshotted yaml](./yaml/deployment.yaml)
and apply as you see fit (like above).

To develop by building and deploying the image quickly, we recommend using [tilt](https://tilt.dev/), via `tilt up`
instead.

## Usage

In either of the run scenarios, your app is listening on port `8080`, and it will observe `Document` events.

Try some of:

```sh
kubectl apply -f yaml/instance-lorem.yaml
kubectl delete doc lorem
kubectl edit doc lorem # change hidden
```

The reconciler will run and write the status object on every change. You should see results in the logs of the pod, or
on the `.status` object outputs of `kubectl get doc -oyaml`.

### Webapp output

The sample web server exposes some example metrics and debug information you can inspect with `curl`.

```sh
$ kubectl apply -f yaml/instance-lorem.yaml
$ curl 0.0.0.0:8080/metrics
# HELP db_controller_reconcile_duration_seconds The duration of reconcile to complete in seconds
# TYPE db_controller_reconcile_duration_seconds histogram
db_controller_reconcile_duration_seconds_bucket{le="0.01"} 1
db_controller_reconcile_duration_seconds_bucket{le="0.1"} 1
db_controller_reconcile_duration_seconds_bucket{le="0.25"} 1
db_controller_reconcile_duration_seconds_bucket{le="0.5"} 1
db_controller_reconcile_duration_seconds_bucket{le="1"} 1
db_controller_reconcile_duration_seconds_bucket{le="5"} 1
db_controller_reconcile_duration_seconds_bucket{le="15"} 1
db_controller_reconcile_duration_seconds_bucket{le="60"} 1
db_controller_reconcile_duration_seconds_bucket{le="+Inf"} 1
db_controller_reconcile_duration_seconds_sum 0.013
db_controller_reconcile_duration_seconds_count 1
# HELP db_controller_reconciliation_errors_total reconciliation errors
# TYPE db_controller_reconciliation_errors_total counter
db_controller_reconciliation_errors_total 0
# HELP db_controller_reconciliations_total reconciliations
# TYPE db_controller_reconciliations_total counter
db_controller_reconciliations_total 1
$ curl 0.0.0.0:8080/
{"last_event":"2019-07-17T22:31:37.591320068Z"}
```

The metrics will be scraped by prometheus if you setup a`ServiceMonitor` for it.

### Events

The example `reconciler` only checks the `.spec.hidden` bool. If it does, it updates the `.status` object to reflect
whether the instance `is_hidden`. It also sends a Kubernetes event associated with the controller. It is visible
at the bottom of `kubectl describe doc samuel`.

To extend this controller for a real-world setting. Consider looking at
the [kube.rs controller guide](https://kube.rs/controllers/intro/).

## Kubernetes In Docker

### Using MicroK8s (for Ubuntu users)

Install:

```shell
sudo snap install microk8s --classic && \
sudo microk8s.enable dns && \
sudo microk8s.enable registry
```

Fetch the configuration and merge it into your `.kube/config` file.

```shell
sudo microk8s.kubectl config view --flatten
```

You can use [kubectx](https://github.com/ahmetb/kubectx) to switch the contexts quickly.

### Using ctlptl and kind

Use [ctlptl](https://github.com/tilt-dev/ctlptl) and [kind](https://kind.sigs.k8s.io/) to create a cluster:

```shell
ctlptl create registry ctlptl-registry --port=5005
ctlptl create cluster kind --registry=ctlptl-registry --name=kind-db-operator
```
