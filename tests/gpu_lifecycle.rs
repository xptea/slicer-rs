#[allow(dead_code)]
#[path = "../src/engine/gpu/mod.rs"]
mod gpu;

use gpu::{
    Capability, CapabilityReport, CapabilityStatus, ContractError, LifecycleState,
    PresentationRoute, PresentationSession, PresentationTarget, ResizeRequest, SurfaceExtent,
};

fn validated_report(route: PresentationRoute) -> CapabilityReport {
    let mut report = CapabilityReport::not_probed();
    for &capability in route.requirements() {
        report.set(capability, CapabilityStatus::Validated);
    }
    report
}

#[test]
fn unprobed_contract_does_not_activate_a_route() {
    let route = PresentationRoute::OffscreenVulkanGpuUiImport;
    let mut session = PresentationSession::new(route, CapabilityReport::not_probed());
    let extent = SurfaceExtent::new(1920, 1080).expect("valid extent");

    assert!(matches!(
        session.activate(
            PresentationTarget::GpuUiImport(gpu::GpuUiImportToken::from_contract_id(7),),
            extent,
        ),
        Err(ContractError::RouteNotReady { .. })
    ));
    assert_eq!(session.state(), LifecycleState::Created);
    assert!(matches!(
        session.route_readiness(),
        gpu::RouteReadiness::NeedsProbe(_)
    ));
}

#[test]
fn minimized_resize_suspends_submission_and_can_resume() {
    let route = PresentationRoute::OffscreenVulkanGpuUiImport;
    let mut session = PresentationSession::new(route, validated_report(route));
    session
        .activate(
            PresentationTarget::GpuUiImport(gpu::GpuUiImportToken::from_contract_id(8)),
            SurfaceExtent::new(1280, 720).expect("valid extent"),
        )
        .expect("synthetic activation");

    session
        .begin_resize(ResizeRequest::Suspended)
        .expect("begin minimize");
    session.complete_resize().expect("complete minimize");
    assert_eq!(session.state(), LifecycleState::Suspended);
    assert_eq!(session.extent(), None);
    assert!(matches!(
        session.submit_frame(&[]),
        Err(ContractError::InvalidTransition { .. })
    ));

    let resumed_extent = SurfaceExtent::new(1280, 720).expect("valid extent");
    session
        .begin_resize(ResizeRequest::Extent(resumed_extent))
        .expect("begin resume");
    session.complete_resize().expect("complete resume");
    assert_eq!(session.state(), LifecycleState::Ready);
    assert_eq!(session.extent(), Some(resumed_extent));
}

#[test]
fn route_requirements_are_not_interchangeable() {
    let mut report = CapabilityReport::not_probed();
    for &capability in PresentationRoute::OwnedVulkanChildSurface.requirements() {
        report.set(capability, CapabilityStatus::Validated);
    }

    assert_eq!(
        report.route_readiness(PresentationRoute::OwnedVulkanChildSurface),
        gpu::RouteReadiness::Ready
    );
    assert!(matches!(
        report.route_readiness(PresentationRoute::OffscreenVulkanGpuUiImport),
        gpu::RouteReadiness::NeedsProbe(missing)
            if missing.contains(&Capability::GpuUiImageImport)
    ));
}
