//! Contract-only Vulkan presentation spike.
//!
//! This module deliberately does not load Vulkan, call an FFI binding, create a
//! device, or own an operating-system handle.  It freezes the route,
//! capability, lifecycle, and resource-retirement contracts that a later
//! backend can implement.  The standalone example and tests include this file
//! explicitly so the production crate remains unwired until a presentation
//! route has been measured and selected.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

/// The two presentation routes from the editor presentation decision gate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PresentationRoute {
    /// An owned Vulkan device presents into the existing native child surface.
    OwnedVulkanChildSurface,
    /// An offscreen Vulkan compositor exports an image into the GPUI renderer.
    OffscreenVulkanGpuUiImport,
}

impl PresentationRoute {
    /// All routes represented by this contract.
    pub const ALL: [Self; 2] = [
        Self::OwnedVulkanChildSurface,
        Self::OffscreenVulkanGpuUiImport,
    ];

    /// Capabilities that must be validated before activation is allowed.
    pub fn requirements(self) -> &'static [Capability] {
        match self {
            Self::OwnedVulkanChildSurface => OWNED_CHILD_REQUIREMENTS,
            Self::OffscreenVulkanGpuUiImport => OFFSCREEN_IMPORT_REQUIREMENTS,
        }
    }
}

/// A capability needed by one or both presentation routes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    /// The Vulkan loader/runtime can be discovered.
    VulkanLoader,
    /// A usable Vulkan physical/logical device can be selected.
    VulkanDevice,
    /// A graphics queue is available for composition work.
    GraphicsQueue,
    /// A queue can present to the native child surface.
    PresentQueue,
    /// Reusable synthetic YUV/RGBA image resources can be provided.
    ReusableImageResources,
    /// The composition shader/pipeline contract is available.
    CompositionPipeline,
    /// The existing native child surface can be used as a presentation target.
    NativeChildSurface,
    /// An offscreen image can be rendered by the Vulkan compositor.
    OffscreenComposition,
    /// GPUI can consume the offscreen image through a proven import contract.
    GpuUiImageImport,
    /// External image-memory ownership can cross the renderer boundary.
    ExternalMemoryImport,
    /// External semaphore/fence synchronization can cross the boundary.
    ExternalSemaphoreInterop,
    /// The Vulkan and GPUI adapters are known to refer to compatible devices.
    AdapterMatch,
    /// Image layouts and ownership transitions are proven across the boundary.
    LayoutTransitionInterop,
}

impl Capability {
    /// Every capability known to this contract.
    pub const ALL: [Self; 13] = [
        Self::VulkanLoader,
        Self::VulkanDevice,
        Self::GraphicsQueue,
        Self::PresentQueue,
        Self::ReusableImageResources,
        Self::CompositionPipeline,
        Self::NativeChildSurface,
        Self::OffscreenComposition,
        Self::GpuUiImageImport,
        Self::ExternalMemoryImport,
        Self::ExternalSemaphoreInterop,
        Self::AdapterMatch,
        Self::LayoutTransitionInterop,
    ];
}

const OWNED_CHILD_REQUIREMENTS: &[Capability] = &[
    Capability::VulkanLoader,
    Capability::VulkanDevice,
    Capability::GraphicsQueue,
    Capability::PresentQueue,
    Capability::ReusableImageResources,
    Capability::CompositionPipeline,
    Capability::NativeChildSurface,
];

const OFFSCREEN_IMPORT_REQUIREMENTS: &[Capability] = &[
    Capability::VulkanLoader,
    Capability::VulkanDevice,
    Capability::GraphicsQueue,
    Capability::ReusableImageResources,
    Capability::CompositionPipeline,
    Capability::OffscreenComposition,
    Capability::GpuUiImageImport,
    Capability::ExternalMemoryImport,
    Capability::ExternalSemaphoreInterop,
    Capability::AdapterMatch,
    Capability::LayoutTransitionInterop,
];

/// Evidence level for a capability.
///
/// `Observed` means a probe saw a potentially usable facility.  It is not
/// enough to activate a route: only `Validated` represents an end-to-end
/// presentation proof.  The default report is `NotProbed` for every entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityStatus {
    /// No runtime or hardware probe has been run.
    NotProbed,
    /// A runtime probe found the facility, but integration is unvalidated.
    Observed,
    /// The facility has passed the route's end-to-end validation.
    Validated,
    /// A probe or known constraint rules the facility out.
    Unavailable { reason: String },
}

impl CapabilityStatus {
    /// Create an unavailable status with an owned explanation.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }
}

/// The current evidence for known GPU capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityReport {
    statuses: BTreeMap<Capability, CapabilityStatus>,
}

impl CapabilityReport {
    /// Create a report that makes no claim about the local runtime or device.
    pub fn not_probed() -> Self {
        let statuses = Capability::ALL
            .into_iter()
            .map(|capability| (capability, CapabilityStatus::NotProbed))
            .collect();
        Self { statuses }
    }

    /// Read the evidence level for one capability.
    pub fn status(&self, capability: Capability) -> &CapabilityStatus {
        self.statuses
            .get(&capability)
            .expect("all known capabilities are present in a report")
    }

    /// Update evidence for one capability.
    pub fn set(&mut self, capability: Capability, status: CapabilityStatus) {
        self.statuses.insert(capability, status);
    }

    /// Reset route-specific evidence after a device loss.
    pub fn reset_route(&mut self, route: PresentationRoute) {
        for &capability in route.requirements() {
            self.set(capability, CapabilityStatus::NotProbed);
        }
    }

    /// Determine whether a route is safe to activate under this evidence.
    pub fn route_readiness(&self, route: PresentationRoute) -> RouteReadiness {
        let unavailable = route
            .requirements()
            .iter()
            .copied()
            .filter(|&capability| {
                matches!(
                    self.status(capability),
                    CapabilityStatus::Unavailable { .. }
                )
            })
            .collect::<Vec<_>>();
        if !unavailable.is_empty() {
            return RouteReadiness::Unavailable(unavailable);
        }

        let unprobed = route
            .requirements()
            .iter()
            .copied()
            .filter(|&capability| matches!(self.status(capability), CapabilityStatus::NotProbed))
            .collect::<Vec<_>>();
        if !unprobed.is_empty() {
            return RouteReadiness::NeedsProbe(unprobed);
        }

        let unvalidated = route
            .requirements()
            .iter()
            .copied()
            .filter(|&capability| matches!(self.status(capability), CapabilityStatus::Observed))
            .collect::<Vec<_>>();
        if !unvalidated.is_empty() {
            return RouteReadiness::NeedsValidation(unvalidated);
        }

        RouteReadiness::Ready
    }
}

/// Activation readiness for one route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteReadiness {
    /// Every requirement is validated.
    Ready,
    /// At least one requirement has not been probed.
    NeedsProbe(Vec<Capability>),
    /// Requirements were observed but not proven end to end.
    NeedsValidation(Vec<Capability>),
    /// At least one requirement is known to be unavailable.
    Unavailable(Vec<Capability>),
}

impl RouteReadiness {
    fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

impl fmt::Display for RouteReadiness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready => formatter.write_str("ready"),
            Self::NeedsProbe(capabilities) => {
                write_capability_list(formatter, "needs probe", capabilities)
            }
            Self::NeedsValidation(capabilities) => {
                write_capability_list(formatter, "needs validation", capabilities)
            }
            Self::Unavailable(capabilities) => {
                write_capability_list(formatter, "unavailable", capabilities)
            }
        }
    }
}

fn write_capability_list(
    formatter: &mut fmt::Formatter<'_>,
    prefix: &str,
    capabilities: &[Capability],
) -> fmt::Result {
    write!(formatter, "{prefix}: ")?;
    for (index, capability) in capabilities.iter().enumerate() {
        if index != 0 {
            formatter.write_str(", ")?;
        }
        write!(formatter, "{capability:?}")?;
    }
    Ok(())
}

/// An opaque contract-only token for the existing native child surface.
///
/// This is an identifier, not an X11 handle or a Vulkan object.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChildSurfaceToken(u64);

impl ChildSurfaceToken {
    /// Construct a token for tests or a future backend adapter.
    pub const fn from_contract_id(id: u64) -> Self {
        Self(id)
    }
}

/// An opaque contract-only token for a future GPUI image-import target.
///
/// This is an identifier, not a raw pointer or an imported image handle.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GpuUiImportToken(u64);

impl GpuUiImportToken {
    /// Construct a token for tests or a future backend adapter.
    pub const fn from_contract_id(id: u64) -> Self {
        Self(id)
    }
}

/// The target shape expected by each presentation route.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PresentationTarget {
    /// Existing native child target.
    ChildSurface(ChildSurfaceToken),
    /// Future GPUI import target.
    GpuUiImport(GpuUiImportToken),
}

impl PresentationTarget {
    fn matches(self, route: PresentationRoute) -> bool {
        matches!(
            (route, self),
            (
                PresentationRoute::OwnedVulkanChildSurface,
                Self::ChildSurface(_)
            ) | (
                PresentationRoute::OffscreenVulkanGpuUiImport,
                Self::GpuUiImport(_)
            )
        )
    }
}

/// A validated, non-zero presentation extent.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SurfaceExtent {
    width: u32,
    height: u32,
}

impl SurfaceExtent {
    /// Construct a non-zero extent.
    pub const fn new(width: u32, height: u32) -> Result<Self, ContractError> {
        if width == 0 || height == 0 {
            return Err(ContractError::InvalidExtent);
        }
        Ok(Self { width, height })
    }

    /// Width in pixels.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub const fn height(self) -> u32 {
        self.height
    }
}

/// A requested resize, including the minimized/suspended state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResizeRequest {
    /// Recreate presentation resources for this extent.
    Extent(SurfaceExtent),
    /// Keep the target attached but suspend frame submission while minimized.
    Suspended,
}

/// Lifecycle states shared by either presentation route.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LifecycleState {
    /// No target or presentation resources have been activated.
    Created,
    /// The route is active and can accept a frame submission.
    Ready,
    /// The target is attached but has no drawable extent.
    Suspended,
    /// A resize is pending safe retirement of old GPU work.
    Resizing,
    /// GPU resources were invalidated and must be released before recovery.
    DeviceLost,
    /// Ordered shutdown is in progress.
    ShuttingDown(ShutdownStage),
    /// The target and all owned resources have been released.
    Shutdown,
}

/// The required order for ending a presentation session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ShutdownStage {
    /// Stop accepting/cancel producers.
    CancelProducers,
    /// Stop audio before waiting on GPU work.
    StopAudio,
    /// Retire every submitted frame before destroying resources.
    RetireGpuWork,
    /// Release imported decoder/frame leases.
    ReleaseImportedFrames,
    /// Destroy pipelines, images, buffers, and other render resources.
    DestroyRenderResources,
    /// Detach/destroy the presentation target last.
    DestroySurface,
}

/// A resource category tracked by the contract-only lifetime ledger.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResourceKind {
    /// A decoder-owned or externally imported frame lease.
    ImportedFrame,
    /// Reusable software-decoded YUV upload storage.
    YuvUpload,
    /// Reusable RGBA upload/storage.
    RgbaImage,
    /// An offscreen composition target.
    CompositionTarget,
    /// A presentable image or swapchain-backed image.
    PresentableImage,
    /// A compiled composition pipeline.
    Pipeline,
}

/// A contract-only resource identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceId(u64);

/// A submitted frame identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SubmissionId(u64);

/// Ownership state of a resource in the lifetime ledger.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResourceState {
    /// Available for a new submission.
    Available,
    /// Referenced by a submitted frame that has not retired.
    InFlight(SubmissionId),
    /// GPU work retired; the owner may release or reuse it.
    Retired,
    /// The device was lost before normal retirement.
    Lost,
    /// Released and no longer owned by the presentation session.
    Released,
}

/// Counts exposed for bounded-resource diagnostics and tests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResourceCounts {
    /// Resources available for reuse.
    pub available: usize,
    /// Resources referenced by submitted work.
    pub in_flight: usize,
    /// Resources whose submitted work has retired.
    pub retired: usize,
    /// Resources invalidated by device loss.
    pub lost: usize,
    /// Resources released from the session.
    pub released: usize,
}

impl ResourceCounts {
    /// Number of resources that are still owned by the session.
    pub const fn live(self) -> usize {
        self.available + self.in_flight + self.retired + self.lost
    }
}

#[derive(Clone, Debug)]
struct ResourceRecord {
    kind: ResourceKind,
    state: ResourceState,
}

/// A small ownership ledger standing in for future Vulkan allocations.
#[derive(Clone, Debug, Default)]
pub struct ResourceRegistry {
    next_id: u64,
    records: BTreeMap<ResourceId, ResourceRecord>,
}

impl ResourceRegistry {
    /// Allocate a new contract-only resource.
    pub fn allocate(&mut self, kind: ResourceKind) -> ResourceId {
        self.next_id = self.next_id.saturating_add(1);
        let id = ResourceId(self.next_id);
        let previous = self.records.insert(
            id,
            ResourceRecord {
                kind,
                state: ResourceState::Available,
            },
        );
        debug_assert!(previous.is_none());
        id
    }

    /// Inspect a resource's ownership state.
    pub fn state(&self, id: ResourceId) -> Option<ResourceState> {
        self.records.get(&id).map(|record| record.state)
    }

    /// Inspect a resource's category.
    pub fn kind(&self, id: ResourceId) -> Option<ResourceKind> {
        self.records.get(&id).map(|record| record.kind)
    }

    /// Return counts for diagnostics or bounded-pool assertions.
    pub fn counts(&self) -> ResourceCounts {
        let mut counts = ResourceCounts::default();
        for record in self.records.values() {
            match record.state {
                ResourceState::Available => counts.available += 1,
                ResourceState::InFlight(_) => counts.in_flight += 1,
                ResourceState::Retired => counts.retired += 1,
                ResourceState::Lost => counts.lost += 1,
                ResourceState::Released => counts.released += 1,
            }
        }
        counts
    }

    fn mark_in_flight(
        &mut self,
        id: ResourceId,
        submission: SubmissionId,
    ) -> Result<(), ContractError> {
        let record = self
            .records
            .get_mut(&id)
            .ok_or(ContractError::UnknownResource(id))?;
        if record.state != ResourceState::Available {
            return Err(ContractError::ResourceNotAvailable {
                id,
                state: record.state,
            });
        }
        record.state = ResourceState::InFlight(submission);
        Ok(())
    }

    fn retire(&mut self, id: ResourceId, submission: SubmissionId) -> Result<(), ContractError> {
        let record = self
            .records
            .get_mut(&id)
            .ok_or(ContractError::UnknownResource(id))?;
        if record.state != ResourceState::InFlight(submission) {
            return Err(ContractError::ResourceNotAvailable {
                id,
                state: record.state,
            });
        }
        record.state = ResourceState::Retired;
        Ok(())
    }

    /// Release a resource after it is available or its work has retired.
    pub fn release(&mut self, id: ResourceId) -> Result<(), ContractError> {
        let record = self
            .records
            .get_mut(&id)
            .ok_or(ContractError::UnknownResource(id))?;
        match record.state {
            ResourceState::Available | ResourceState::Retired | ResourceState::Lost => {
                record.state = ResourceState::Released;
                Ok(())
            }
            state => Err(ContractError::ResourceNotAvailable { id, state }),
        }
    }

    fn release_retired(&mut self) -> usize {
        let mut released = 0;
        for record in self.records.values_mut() {
            if record.state == ResourceState::Retired {
                record.state = ResourceState::Released;
                released += 1;
            }
        }
        released
    }

    fn release_kind(&mut self, kind: ResourceKind) -> Result<usize, ContractError> {
        let in_flight = self
            .records
            .values()
            .filter(|record| {
                record.kind == kind && matches!(record.state, ResourceState::InFlight(_))
            })
            .count();
        if in_flight != 0 {
            return Err(ContractError::GpuWorkOutstanding {
                operation: "release resources",
                count: in_flight,
            });
        }

        let mut released = 0;
        for record in self.records.values_mut() {
            if record.kind == kind
                && matches!(
                    record.state,
                    ResourceState::Available | ResourceState::Retired | ResourceState::Lost
                )
            {
                record.state = ResourceState::Released;
                released += 1;
            }
        }
        Ok(released)
    }

    fn release_all(&mut self) -> Result<usize, ContractError> {
        let in_flight = self
            .records
            .values()
            .filter(|record| matches!(record.state, ResourceState::InFlight(_)))
            .count();
        if in_flight != 0 {
            return Err(ContractError::GpuWorkOutstanding {
                operation: "destroy render resources",
                count: in_flight,
            });
        }

        let mut released = 0;
        for record in self.records.values_mut() {
            if matches!(
                record.state,
                ResourceState::Available | ResourceState::Retired | ResourceState::Lost
            ) {
                record.state = ResourceState::Released;
                released += 1;
            }
        }
        Ok(released)
    }

    fn invalidate_for_device_loss(&mut self) -> usize {
        let mut lost = 0;
        for record in self.records.values_mut() {
            if record.state != ResourceState::Released {
                record.state = ResourceState::Lost;
                lost += 1;
            }
        }
        lost
    }

    fn release_lost(&mut self) -> usize {
        let mut released = 0;
        for record in self.records.values_mut() {
            if record.state == ResourceState::Lost {
                record.state = ResourceState::Released;
                released += 1;
            }
        }
        released
    }
}

/// Errors raised when a caller violates the presentation contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContractError {
    /// An operation is not valid from the current lifecycle state.
    InvalidTransition {
        operation: &'static str,
        state: LifecycleState,
    },
    /// Activation or recovery was attempted without end-to-end capability proof.
    RouteNotReady {
        route: PresentationRoute,
        readiness: RouteReadiness,
    },
    /// The supplied target does not match the selected route.
    TargetRouteMismatch {
        route: PresentationRoute,
        target: PresentationTarget,
    },
    /// A surface extent must have non-zero dimensions.
    InvalidExtent,
    /// An operation requires all submitted work to be retired first.
    GpuWorkOutstanding {
        operation: &'static str,
        count: usize,
    },
    /// A resource identifier is not in this session.
    UnknownResource(ResourceId),
    /// A resource is not in a state that permits the requested operation.
    ResourceNotAvailable {
        id: ResourceId,
        state: ResourceState,
    },
    /// A submission identifier is not in this session.
    UnknownSubmission(SubmissionId),
    /// A resource was listed twice in one frame submission.
    DuplicateResource(ResourceId),
    /// Capability evidence may only change before activation or during recovery.
    CapabilityMutationNotAllowed { state: LifecycleState },
    /// Recovery requires all invalidated resources to be released first.
    RecoveryResourcesOutstanding { count: usize },
    /// Shutdown reached its final stage with resources still owned.
    ShutdownResourcesOutstanding { count: usize },
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransition { operation, state } => {
                write!(formatter, "cannot {operation} while in {state:?}")
            }
            Self::RouteNotReady { route, readiness } => {
                write!(formatter, "cannot activate {route:?}: {readiness}")
            }
            Self::TargetRouteMismatch { route, target } => {
                write!(
                    formatter,
                    "target {target:?} does not match route {route:?}"
                )
            }
            Self::InvalidExtent => formatter.write_str("surface extent must be non-zero"),
            Self::GpuWorkOutstanding { operation, count } => {
                write!(
                    formatter,
                    "cannot {operation}: {count} GPU resources are in flight"
                )
            }
            Self::UnknownResource(id) => write!(formatter, "unknown resource {id:?}"),
            Self::ResourceNotAvailable { id, state } => {
                write!(formatter, "resource {id:?} is {state:?}")
            }
            Self::UnknownSubmission(id) => write!(formatter, "unknown submission {id:?}"),
            Self::DuplicateResource(id) => write!(formatter, "resource {id:?} was submitted twice"),
            Self::CapabilityMutationNotAllowed { state } => {
                write!(formatter, "capabilities cannot change while in {state:?}")
            }
            Self::RecoveryResourcesOutstanding { count } => {
                write!(
                    formatter,
                    "cannot recover with {count} resources still owned"
                )
            }
            Self::ShutdownResourcesOutstanding { count } => {
                write!(formatter, "shutdown has {count} resources still owned")
            }
        }
    }
}

impl Error for ContractError {}

/// A presentation session implementing the route and ownership state machine.
///
/// It stores no Vulkan object.  `PresentationTarget`, `ResourceId`, and
/// `SubmissionId` are opaque contract tokens for tests and future adapters.
#[derive(Clone, Debug)]
pub struct PresentationSession {
    route: PresentationRoute,
    capabilities: CapabilityReport,
    state: LifecycleState,
    target: Option<PresentationTarget>,
    extent: Option<SurfaceExtent>,
    pending_resize: Option<ResizeRequest>,
    resources: ResourceRegistry,
    submissions: BTreeMap<SubmissionId, Vec<ResourceId>>,
    next_submission: u64,
    generation: u64,
    shutdown_trace: Vec<ShutdownStage>,
}

impl PresentationSession {
    /// Create an inactive session with no claimed OS/GPU resources.
    pub fn new(route: PresentationRoute, capabilities: CapabilityReport) -> Self {
        Self {
            route,
            capabilities,
            state: LifecycleState::Created,
            target: None,
            extent: None,
            pending_resize: None,
            resources: ResourceRegistry::default(),
            submissions: BTreeMap::new(),
            next_submission: 0,
            generation: 0,
            shutdown_trace: Vec::new(),
        }
    }

    /// Selected route.
    pub const fn route(&self) -> PresentationRoute {
        self.route
    }

    /// Current lifecycle state.
    pub const fn state(&self) -> LifecycleState {
        self.state
    }

    /// Current capability evidence.
    pub fn capabilities(&self) -> &CapabilityReport {
        &self.capabilities
    }

    /// Current route readiness.
    pub fn route_readiness(&self) -> RouteReadiness {
        self.capabilities.route_readiness(self.route)
    }

    /// Current opaque presentation target, if activated.
    pub const fn target(&self) -> Option<PresentationTarget> {
        self.target
    }

    /// Current drawable extent, if not suspended.
    pub const fn extent(&self) -> Option<SurfaceExtent> {
        self.extent
    }

    /// Monotonic generation for resize/device-loss/recovery invalidation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Resource ownership ledger.
    pub const fn resources(&self) -> &ResourceRegistry {
        &self.resources
    }

    /// Ordered shutdown stages entered by this session.
    pub fn shutdown_trace(&self) -> &[ShutdownStage] {
        &self.shutdown_trace
    }

    /// Update capability evidence before activation or while recovering.
    pub fn set_capability(
        &mut self,
        capability: Capability,
        status: CapabilityStatus,
    ) -> Result<(), ContractError> {
        if !matches!(
            self.state,
            LifecycleState::Created | LifecycleState::DeviceLost
        ) {
            return Err(ContractError::CapabilityMutationNotAllowed { state: self.state });
        }
        self.capabilities.set(capability, status);
        Ok(())
    }

    /// Attach a route-matching target and activate the contract.
    pub fn activate(
        &mut self,
        target: PresentationTarget,
        extent: SurfaceExtent,
    ) -> Result<(), ContractError> {
        self.require_state("activate", &[LifecycleState::Created])?;
        self.ensure_route_ready()?;
        if !target.matches(self.route) {
            return Err(ContractError::TargetRouteMismatch {
                route: self.route,
                target,
            });
        }
        self.target = Some(target);
        self.extent = Some(extent);
        self.state = LifecycleState::Ready;
        self.bump_generation();
        Ok(())
    }

    /// Allocate a contract-only resource while the route owns a target.
    pub fn allocate_resource(&mut self, kind: ResourceKind) -> Result<ResourceId, ContractError> {
        if !matches!(self.state, LifecycleState::Ready | LifecycleState::Resizing) {
            return Err(ContractError::InvalidTransition {
                operation: "allocate a resource",
                state: self.state,
            });
        }
        Ok(self.resources.allocate(kind))
    }

    /// Inspect one resource owned by this session.
    pub fn resource_state(&self, id: ResourceId) -> Option<ResourceState> {
        self.resources.state(id)
    }

    /// Submit a frame and mark its resources in flight.
    pub fn submit_frame(
        &mut self,
        resources: &[ResourceId],
    ) -> Result<SubmissionId, ContractError> {
        self.require_state("submit a frame", &[LifecycleState::Ready])?;

        let mut unique = BTreeSet::new();
        for &id in resources {
            if !unique.insert(id) {
                return Err(ContractError::DuplicateResource(id));
            }
            match self.resources.state(id) {
                Some(ResourceState::Available) => {}
                Some(state) => return Err(ContractError::ResourceNotAvailable { id, state }),
                None => return Err(ContractError::UnknownResource(id)),
            }
        }

        self.next_submission = self.next_submission.saturating_add(1);
        let submission = SubmissionId(self.next_submission);
        for id in resources {
            self.resources.mark_in_flight(*id, submission)?;
        }
        self.submissions.insert(submission, resources.to_vec());
        Ok(submission)
    }

    /// Retire a submitted frame after its queue completion is known.
    pub fn retire_submission(&mut self, submission: SubmissionId) -> Result<(), ContractError> {
        if matches!(
            self.state,
            LifecycleState::Created | LifecycleState::DeviceLost | LifecycleState::Shutdown
        ) {
            return Err(ContractError::InvalidTransition {
                operation: "retire a submission",
                state: self.state,
            });
        }
        let resources = self
            .submissions
            .get(&submission)
            .cloned()
            .ok_or(ContractError::UnknownSubmission(submission))?;
        for &id in &resources {
            if self.resources.state(id) != Some(ResourceState::InFlight(submission)) {
                return Err(ContractError::ResourceNotAvailable {
                    id,
                    state: self.resources.state(id).unwrap_or(ResourceState::Released),
                });
            }
        }
        self.submissions.remove(&submission);
        for id in resources {
            self.resources.retire(id, submission)?;
        }
        Ok(())
    }

    /// Release every retired resource that is ready for reuse.
    pub fn release_retired_resources(&mut self) -> Result<usize, ContractError> {
        if self.state == LifecycleState::Shutdown {
            return Err(ContractError::InvalidTransition {
                operation: "release retired resources",
                state: self.state,
            });
        }
        Ok(self.resources.release_retired())
    }

    /// Release one resource after retirement or explicit non-use.
    pub fn release_resource(&mut self, id: ResourceId) -> Result<(), ContractError> {
        if self.state == LifecycleState::Shutdown {
            return Err(ContractError::InvalidTransition {
                operation: "release a resource",
                state: self.state,
            });
        }
        self.resources.release(id)
    }

    /// Start a resize; completion is blocked until old GPU work retires.
    pub fn begin_resize(&mut self, request: ResizeRequest) -> Result<(), ContractError> {
        self.require_state(
            "begin a resize",
            &[LifecycleState::Ready, LifecycleState::Suspended],
        )?;
        self.pending_resize = Some(request);
        self.state = LifecycleState::Resizing;
        Ok(())
    }

    /// Complete a pending resize once no submission remains in flight.
    pub fn complete_resize(&mut self) -> Result<(), ContractError> {
        self.require_state("complete a resize", &[LifecycleState::Resizing])?;
        self.require_no_in_flight("complete a resize")?;
        let request = self
            .pending_resize
            .take()
            .expect("resizing state always has a pending request");
        match request {
            ResizeRequest::Extent(extent) => {
                self.extent = Some(extent);
                self.state = LifecycleState::Ready;
            }
            ResizeRequest::Suspended => {
                self.extent = None;
                self.state = LifecycleState::Suspended;
            }
        }
        self.bump_generation();
        Ok(())
    }

    /// Mark all owned GPU resources lost without pretending they retired.
    pub fn mark_device_lost(&mut self) -> Result<(), ContractError> {
        if !matches!(
            self.state,
            LifecycleState::Ready | LifecycleState::Suspended | LifecycleState::Resizing
        ) {
            return Err(ContractError::InvalidTransition {
                operation: "mark the device lost",
                state: self.state,
            });
        }
        self.resources.invalidate_for_device_loss();
        self.submissions.clear();
        self.pending_resize = None;
        self.capabilities.reset_route(self.route);
        self.state = LifecycleState::DeviceLost;
        self.bump_generation();
        Ok(())
    }

    /// Release resources invalidated by a device loss before recreating it.
    pub fn release_lost_resources(&mut self) -> Result<usize, ContractError> {
        self.require_state("release lost resources", &[LifecycleState::DeviceLost])?;
        Ok(self.resources.release_lost())
    }

    /// Reattach a target after loss, requiring fresh route validation.
    pub fn recover(
        &mut self,
        target: PresentationTarget,
        extent: SurfaceExtent,
    ) -> Result<(), ContractError> {
        self.require_state("recover the device", &[LifecycleState::DeviceLost])?;
        self.ensure_route_ready()?;
        let live = self.resources.counts().live();
        if live != 0 {
            return Err(ContractError::RecoveryResourcesOutstanding { count: live });
        }
        if !target.matches(self.route) {
            return Err(ContractError::TargetRouteMismatch {
                route: self.route,
                target,
            });
        }
        self.target = Some(target);
        self.extent = Some(extent);
        self.state = LifecycleState::Ready;
        self.bump_generation();
        Ok(())
    }

    /// Begin the ordered shutdown sequence.
    pub fn begin_shutdown(&mut self) -> Result<(), ContractError> {
        if self.state == LifecycleState::Shutdown {
            return Ok(());
        }
        if matches!(self.state, LifecycleState::ShuttingDown(_)) {
            return Err(ContractError::InvalidTransition {
                operation: "begin shutdown",
                state: self.state,
            });
        }
        self.pending_resize = None;
        self.state = LifecycleState::ShuttingDown(ShutdownStage::CancelProducers);
        self.shutdown_trace.clear();
        self.shutdown_trace.push(ShutdownStage::CancelProducers);
        Ok(())
    }

    /// Advance exactly one ordered shutdown stage.
    pub fn advance_shutdown(&mut self) -> Result<(), ContractError> {
        let stage = match self.state {
            LifecycleState::ShuttingDown(stage) => stage,
            state => {
                return Err(ContractError::InvalidTransition {
                    operation: "advance shutdown",
                    state,
                });
            }
        };

        match stage {
            ShutdownStage::CancelProducers => self.enter_shutdown_stage(ShutdownStage::StopAudio),
            ShutdownStage::StopAudio => self.enter_shutdown_stage(ShutdownStage::RetireGpuWork),
            ShutdownStage::RetireGpuWork => {
                self.require_no_in_flight("advance shutdown")?;
                self.enter_shutdown_stage(ShutdownStage::ReleaseImportedFrames);
            }
            ShutdownStage::ReleaseImportedFrames => {
                self.require_no_in_flight("release imported frames")?;
                self.resources.release_kind(ResourceKind::ImportedFrame)?;
                self.enter_shutdown_stage(ShutdownStage::DestroyRenderResources);
            }
            ShutdownStage::DestroyRenderResources => {
                self.require_no_in_flight("destroy render resources")?;
                self.resources.release_all()?;
                self.enter_shutdown_stage(ShutdownStage::DestroySurface);
            }
            ShutdownStage::DestroySurface => {
                let live = self.resources.counts().live();
                if live != 0 {
                    return Err(ContractError::ShutdownResourcesOutstanding { count: live });
                }
                self.target = None;
                self.extent = None;
                self.pending_resize = None;
                self.state = LifecycleState::Shutdown;
            }
        }
        Ok(())
    }

    fn ensure_route_ready(&self) -> Result<(), ContractError> {
        let readiness = self.route_readiness();
        if readiness.is_ready() {
            Ok(())
        } else {
            Err(ContractError::RouteNotReady {
                route: self.route,
                readiness,
            })
        }
    }

    fn require_state(
        &self,
        operation: &'static str,
        allowed: &[LifecycleState],
    ) -> Result<(), ContractError> {
        if allowed.contains(&self.state) {
            Ok(())
        } else {
            Err(ContractError::InvalidTransition {
                operation,
                state: self.state,
            })
        }
    }

    fn require_no_in_flight(&self, operation: &'static str) -> Result<(), ContractError> {
        let count = self.resources.counts().in_flight;
        if count == 0 {
            Ok(())
        } else {
            Err(ContractError::GpuWorkOutstanding { operation, count })
        }
    }

    fn enter_shutdown_stage(&mut self, stage: ShutdownStage) {
        self.state = LifecycleState::ShuttingDown(stage);
        self.shutdown_trace.push(stage);
    }

    fn bump_generation(&mut self) {
        self.generation = self.generation.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validated_report(route: PresentationRoute) -> CapabilityReport {
        let mut report = CapabilityReport::not_probed();
        for &capability in route.requirements() {
            report.set(capability, CapabilityStatus::Validated);
        }
        report
    }

    fn active_child_session() -> PresentationSession {
        let route = PresentationRoute::OwnedVulkanChildSurface;
        let mut session = PresentationSession::new(route, validated_report(route));
        let extent = SurfaceExtent::new(1280, 720).expect("valid extent");
        session
            .activate(
                PresentationTarget::ChildSurface(ChildSurfaceToken::from_contract_id(1)),
                extent,
            )
            .expect("synthetic activation");
        session
    }

    #[test]
    fn capability_evidence_requires_probe_then_validation() {
        let route = PresentationRoute::OwnedVulkanChildSurface;
        let mut report = CapabilityReport::not_probed();
        assert!(matches!(
            report.route_readiness(route),
            RouteReadiness::NeedsProbe(_)
        ));

        report.set(route.requirements()[0], CapabilityStatus::Observed);
        assert!(matches!(
            report.route_readiness(route),
            RouteReadiness::NeedsProbe(_)
        ));
        for &capability in route.requirements().iter().skip(1) {
            report.set(capability, CapabilityStatus::Observed);
        }
        assert!(matches!(
            report.route_readiness(route),
            RouteReadiness::NeedsValidation(_)
        ));

        report.set(route.requirements()[0], CapabilityStatus::Validated);
        assert!(matches!(
            report.route_readiness(route),
            RouteReadiness::NeedsValidation(_)
        ));
        for &capability in route.requirements().iter().skip(1) {
            report.set(capability, CapabilityStatus::Validated);
        }
        assert_eq!(report.route_readiness(route), RouteReadiness::Ready);

        report.set(
            Capability::PresentQueue,
            CapabilityStatus::unavailable("no presentation queue"),
        );
        assert!(matches!(
            report.route_readiness(route),
            RouteReadiness::Unavailable(_)
        ));
    }

    #[test]
    fn resize_waits_for_submission_retirement() {
        let mut session = active_child_session();
        let frame = session
            .allocate_resource(ResourceKind::ImportedFrame)
            .expect("resource allocation");
        let submission = session.submit_frame(&[frame]).expect("submission");
        let extent = SurfaceExtent::new(640, 360).expect("valid extent");

        session
            .begin_resize(ResizeRequest::Extent(extent))
            .expect("begin resize");
        assert_eq!(session.state(), LifecycleState::Resizing);
        assert!(matches!(
            session.complete_resize(),
            Err(ContractError::GpuWorkOutstanding { .. })
        ));

        session
            .retire_submission(submission)
            .expect("retire submission");
        session.complete_resize().expect("complete resize");
        assert_eq!(session.state(), LifecycleState::Ready);
        assert_eq!(session.extent(), Some(extent));
        assert_eq!(session.resource_state(frame), Some(ResourceState::Retired));
    }

    #[test]
    fn device_loss_invalidates_and_requires_fresh_capabilities() {
        let mut session = active_child_session();
        let resource = session
            .allocate_resource(ResourceKind::CompositionTarget)
            .expect("resource allocation");
        let submission = session.submit_frame(&[resource]).expect("submission");
        assert_eq!(submission, SubmissionId(1));

        session.mark_device_lost().expect("device loss");
        assert_eq!(session.state(), LifecycleState::DeviceLost);
        assert_eq!(session.resource_state(resource), Some(ResourceState::Lost));
        assert!(matches!(
            session.recover(
                PresentationTarget::ChildSurface(ChildSurfaceToken::from_contract_id(2)),
                SurfaceExtent::new(1280, 720).expect("valid extent"),
            ),
            Err(ContractError::RouteNotReady { .. })
        ));

        assert_eq!(session.release_lost_resources().expect("release lost"), 1);
        let route = session.route();
        for &capability in route.requirements() {
            session
                .set_capability(capability, CapabilityStatus::Validated)
                .expect("re-probe during recovery");
        }
        session
            .recover(
                PresentationTarget::ChildSurface(ChildSurfaceToken::from_contract_id(2)),
                SurfaceExtent::new(1280, 720).expect("valid extent"),
            )
            .expect("recover");
        assert_eq!(session.state(), LifecycleState::Ready);
        assert_eq!(session.resources().counts().live(), 0);
    }

    #[test]
    fn shutdown_enforces_resource_lifetime_order() {
        let mut session = active_child_session();
        let frame = session
            .allocate_resource(ResourceKind::ImportedFrame)
            .expect("frame allocation");
        let pipeline = session
            .allocate_resource(ResourceKind::Pipeline)
            .expect("pipeline allocation");
        let submission = session.submit_frame(&[frame]).expect("submission");

        session.begin_shutdown().expect("begin shutdown");
        session.advance_shutdown().expect("stop audio");
        session.advance_shutdown().expect("enter retirement");
        assert!(matches!(
            session.advance_shutdown(),
            Err(ContractError::GpuWorkOutstanding { .. })
        ));

        session
            .retire_submission(submission)
            .expect("retire before destroy");
        session
            .advance_shutdown()
            .expect("enter imported-frame release");
        session.advance_shutdown().expect("release imported frames");
        assert_eq!(session.resource_state(frame), Some(ResourceState::Released));
        session
            .advance_shutdown()
            .expect("destroy render resources");
        assert_eq!(
            session.resource_state(pipeline),
            Some(ResourceState::Released)
        );
        session.advance_shutdown().expect("destroy surface");
        assert_eq!(session.state(), LifecycleState::Shutdown);
        assert_eq!(session.target(), None);
        assert_eq!(session.resources().counts().live(), 0);
        assert_eq!(
            session.shutdown_trace(),
            &[
                ShutdownStage::CancelProducers,
                ShutdownStage::StopAudio,
                ShutdownStage::RetireGpuWork,
                ShutdownStage::ReleaseImportedFrames,
                ShutdownStage::DestroyRenderResources,
                ShutdownStage::DestroySurface,
            ]
        );
    }
}
