# Network Simulation Testing

Status: partially implemented.

## Current state

The `iroh-live/tests/e2e.rs` integration tests exercise the full
publish-subscribe pipeline over real QUIC connections using synthetic
`NetworkSignals` injection. The `adaptive_rendition_switching` test
verifies that the adaptation algorithm correctly downgrades and upgrades
renditions when signal values change.

This approach tests the adaptation logic end-to-end without requiring
Linux network namespaces or special privileges.

## Patchbay integration (future)

[patchbay](https://crates.io/crates/patchbay) (MIT/Apache-2.0, by
n0-computer) provides Linux network-namespace labs with configurable NAT,
routing, and link impairment (latency, loss, bandwidth, jitter).

### What patchbay would add

- **Realistic impairment**: packet loss and delay applied at the kernel
  level (tc/netem), affecting the actual QUIC congestion controller rather
  than just the signal values we feed to the adaptation algorithm.
- **NAT traversal testing**: verify that live sessions work through Home,
  CGNAT, Corporate, and Hotel NAT topologies.
- **Relay fallback**: confirm that sessions fall back to relay when
  direct connectivity fails.
- **Dynamic degradation**: change link conditions mid-session and verify
  that the media pipeline degrades gracefully (rendition switching, playout
  clock adjustment, FEC activation).

### Integration sketch

```rust
#[cfg(target_os = "linux")]
#[tokio::test]
async fn adaptive_under_real_loss() {
    let lab = patchbay::Lab::new().await?;
    let router = lab.add_router("home")
        .preset(patchbay::RouterPreset::Home)
        .build().await?;
    let pub_dev = lab.add_device("publisher")
        .iface("eth0", router.id(), Some(LinkCondition::Lan))
        .build().await?;
    let sub_dev = lab.add_device("subscriber")
        .iface("eth0", router.id(), Some(LinkCondition::Lan))
        .build().await?;

    // Spawn publisher inside pub_dev namespace.
    let pub_handle = pub_dev.spawn(async { /* bind endpoint, publish */ })?;
    // Spawn subscriber inside sub_dev namespace.
    let sub_handle = sub_dev.spawn(async { /* bind endpoint, subscribe */ })?;

    // Degrade the subscriber link mid-session.
    sub_dev.set_link_condition("eth0", Some(LinkCondition::Mobile3G)).await?;
    // Assert: adaptation downgrades rendition within N seconds.

    // Restore good link.
    sub_dev.set_link_condition("eth0", Some(LinkCondition::Lan)).await?;
    // Assert: adaptation upgrades rendition.
}
```

### Notes 

- Patchbay requires Linux user namespaces (`patchbay::init_userns()`).
  CI runners must support unprivileged user namespaces.
- Tests should initially be `#[serial_test::serial]` to not exhaust fd and namespace limits on ci runners.

### Priority

Low-medium. The synthetic signal injection already covers the adaptation
algorithm. Patchbay adds value for testing:

1. QUIC congestion controller behavior under real loss
2. NAT traversal and relay fallback
3. End-to-end latency budgets under impaired links

These are important for production readiness but not blocking for the
current development phase.
