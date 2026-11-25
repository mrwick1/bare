# Enable browser shared memory for isolated navigations

Commit the policy of the final navigation response before scripts run. Use a fresh agent-cluster identity on each isolated document. Keep the policy on resource clients, including clients reused by network workers. Check responses again when a resource is served from cache or follows a redirect. Check CORS for module loads and prevent a classic-script preload from supplying a module.

Run dedicated workers on separate threads for isolated agents. Send only structured records through bounded queues. Share a backing block within the same agent cluster. Resolve asynchronous waits on the owning scheduler. Cancel blocking waits on termination. Deliver messageerror when an incoming message cannot be cloned.

The focused release selection passes 66 tests with two ignored. The all-target checks pass for bare, bare-net, bare-vex, and bare-js-api. Loopback tests cover buffered and streamed navigation, CORP, CORS, modules, cache, redirects, frame and worker policy, and navigation to a plain document. Scheduler tests cover timeout cancellation and cross-cluster delivery.

Compare release builds against main at `56bab65a` on archMachine. Use the minimum of three runs per binary and allocator. The six session_load and clone rows range from -0.052% to +0.054% in user instructions. The five interp_load cases change by at most +0.002% under glibc and jemalloc. Every measured row is within the +0.5% limit under both allocators.

This release covers top-level require-corp isolation and eligible dedicated workers. Credentialless and iframe shared-memory exposure remain separate. The HTTP worker entry requires a direct response. The full Web Platform Tests suite, Test262, external browser journeys, and Windows were outside this run.
