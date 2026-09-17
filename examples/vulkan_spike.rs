//! Contract-only Vulkan presentation spike.
//!
//! This example intentionally performs no Vulkan probing.  It is useful for
//! compiling and inspecting the route/capability contract while the repository
//! has no direct Vulkan dependency or selected production presentation route.

#[allow(dead_code)]
#[path = "../src/engine/gpu/mod.rs"]
mod gpu;

use gpu::{CapabilityReport, LifecycleState, PresentationRoute, PresentationSession};

fn main() {
    println!("Slicer Vulkan presentation contract spike");
    println!("No Vulkan loader, device, swapchain, or hardware probe is run.");

    for route in PresentationRoute::ALL {
        let session = PresentationSession::new(route, CapabilityReport::not_probed());
        println!(
            "route={route:?} readiness={} lifecycle={:?}",
            session.route_readiness(),
            session.state()
        );
    }

    debug_assert_eq!(
        PresentationSession::new(
            PresentationRoute::OwnedVulkanChildSurface,
            CapabilityReport::not_probed(),
        )
        .state(),
        LifecycleState::Created
    );
}
