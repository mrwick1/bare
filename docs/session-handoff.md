# Paused bare login work — 2026-11-05

## Hold

Arjun said: “pause all ill start new chat , keep all the documented”. All work is paused. Do not resume tests, delegation, edits, pushes or merges until Arjun resumes. The four latest delegated tasks were stopped through task_cancel; all returned completed. All parent build and load sessions have finished. No PR watch is active.

## Goal and latest result

The goal remains actual staging login E2E in bare, with no external crates. It is incomplete. No authenticated journey or GUI result in bare is established. Public cached bundles run only in bare over loopback, never Node. Tiny authored Node oracle cases are allowed.

Main is 29fc42028688ee2f6e33aff45920e23977dc128b, the merge of PR116: https://github.com/mrwick1/bare/pull/116. Main has native submit activation and offset dimensions. It still stops at TraversalDepth on the full saved login. The clean primary checkout is /home/mrwick/coding/personal/projects/bare.

The pending statement-machine integration changes that stop to CallDepth. It still has no form. No guard was raised, reset, discounted or bypassed.

## Worktree and commits

Continue from /home/mrwick/coding/worktrees/bare/statement-machine-review, branch statement-machine-review. It is based on 29fc420. This branch has no remote push or PR.

- 53f4fca: cherry-pick of worker e5cb729. Execute compound synchronous statements on explicit bounded heap frames.
- 81967aa: parent correction. A terminating do-while no longer pays for an iteration that never starts. The new regression failed first, with 5 steps versus 4, then passed.
- The final pause commit includes the sticky Promise-job regression and this handoff. Read git log for its exact SHA.

The original worker commit is e5cb729 on t3/28744cf6, base5795b6c. Keep it as evidence; do not pick it again over 53f4fca.

## Pending implementation

crates/js/src/eval_statements.rs replaces the recursive compound-statement execution path. Block, While, DoWhile, For, Switch, Try/Catch/Finally and Labeled use local SFrame buffers. If is a tail step. ForIn and ForOf retain their guarded helper and call the new machine for their bodies. Expressions, constructors, functions, patterns, preflight walks and async paths keep their native guards.

EvalLimits::max_statement_frames defaults to512 and clamps to768. LimitKind::StatementFrames is new. One cumulative count spans nested synchronous JS calls. SessionConfig::new selects768. Existing session traversal/call limits remain768/128; standalone defaults remain512/100. A frame cap is checked before a push. Every exit restores environments, strictness and frame count. Resource faults skip catch, finally and IteratorClose. collect refuses while statement frames remain active.

Frame buffers are local to a synchronous run. At most16 empty buffers of capacity at most64 are retained for reuse. No machine frame survives a Promise job. Saved finally return values cannot encounter collection during execution. Cross-script callbacks start another machine run and restore their caller's script.

Changed files: crates/js/src/eval.rs, eval_statements.rs, eval_statements_tests.rs, eval_depth_tests.rs; crates/bare/src/browser_session.rs; tools/fixtures/js/statements-cases.txt and .expected; tools/js/statements_check.sh. This pause adds documentation only beyond the tested Promise regression.

## Validation already complete

- Before the parent correction, the assembled workspace passed: bare656, JS531, plus all other crates and the compile-fail doctest. No failure occurred in that workspace run.
- After the do-while correction, debug bare656 and JS532 pass, plus4 HTTPS integrations. Release JS532 passes. Release bare656 passes with --test-threads=1. Four bare tests and one JS test are ignored.
- The first parallel release bare run failed browser_fetch_tests::reused_connections_and_posts_are_never_resent. Its /close-after request log had no later expected request. It passed alone and in the serial full rerun. This is the previously observed socket test, but its cause remains unverified. Do not claim the statement change is proven unrelated.
- The final added test a_statement_frame_fault_poisoned_job_is_not_replayed passes in debug and release. It was added after those full suites. It checks one execution, sticky later drains even after raising the frame budget, zero later jobs, no catch/finally, counter cleanup and collect. Full JS count is now533 if all tests run; that full count was not rerun.
- All65 statements oracle cases match Node after the do-while correction. Node ran authored tiny programs only. The worker compared all26 fixture files against old and new evaluator binaries and reported identical outputs.
- All24 release eval_load model rows pass at1x/10x/100x. The harness uses16MiB VM threads,1GiB address space and45s per process. These are regression checks, not an8MiB stack proof. Peak RSS is about537MiB in closures100x. Other largest outputs: objects100x3.3s, throws100x3.0s. No before/after speed claim comes from the parent runs.
- Worker depth/statement cases passed on actual8MiB release and64MiB debug threads. The worker ran70 fresh processes,35 per profile. The brief allowed fewer than40 total; it misread that as per profile. Record the deviation. These samples do not prove stack safety for all programs.
- Parent clippy on bare-js and bare all targets passes with older warnings elsewhere, including main.rs:68. No warning was reported in changed code.
- The do-while test initially did not compile because Ctl lacks Debug and unwrap required it. That test-only error was corrected before observing the intended runtime failure.

Relevant parent logs: /tmp/bare-statements-workspace.log, /tmp/bare-statements-final-debug.log, /tmp/bare-statements-final-release.log (the failed parallel release), /tmp/bare-statements-fetch-alone.log, /tmp/bare-statements-release-serial.log, /tmp/bare-statements-release-js.log, /tmp/bare-statements-do-red.log, /tmp/bare-statements-final-js.log, /tmp/bare-statements-oracle.log, /tmp/bare-statements-sticky-debug.log, /tmp/bare-statements-sticky-release.log, /tmp/bare-statements-load.log, /tmp/bare-statements-clippy.log. These files are temporary.

## Full cached run

The copied public cache is /tmp/bare-login-assets-_l1_jssr, with15JS and2CSS files. All17 were matched against the manifest earlier. /tmp/bare-login-assets-path stores this path. The tracked HTML is tools/fixtures/staging/login.html. The old /tmp/bare-bootstrap-with-css uses cross-session symlinks; do not depend on it.

Build outside runtime caps with CARGO_BUILD_JOBS=2. The already-run command was:

    (ulimit -v 1048576; timeout 45 target/release/examples/bootstrap_check /tmp/bare-login-assets-_l1_jssr)

The example creates an8MiB release or64MiB debug VM thread. Its default selector is form; do not append a stray dot as a selector. The login module does create a form, so this readiness predicate remains valid.

Observed output at53f4fca:

    stop=Page(Fatal(Limit("CallDepth"))) documents=1 tasks=21
    scripts=21 throws=0 load_errors=0 fetches=17 bytes=575754
    css_loaded=2 css_failed=0 css_bytes=106229
    required_selector_present=false unhandled_rejections=0

The full page was not rerun after81967aa by the parent. The diagnosis worker ran the assembled corrected tree and reproduced the same stop. Parent output is /tmp/bare-statements-bootstrap.log.

## Latest CallDepth diagnosis

Worker task036af211-6ceb-4c24-9f8c-4bf4bd121190 finished before the pause. Its isolated checkout cherry-picked the integration as3a458f2 and8226945 on29fc420. It reports a clean tree at8226945, with probes removed. No product fix, push or PR was made.

Verified by its release probe:

- call_fn_inner refuses atdepth128. Native traversal frames503; statement frames330. Neither frame cap is binding.
- There are127 tracked call_fn entries:126 script calls and1 bound call. One extra depth unit is outside the probe. Hypothesis: host_scope.
- Active script chain: script8/fid505, bound call, script7/fid396,403,431,430, then about60 alternating pairs fid365/fid367.
- Callee handle2557 appears60 times with first argument handle4862 every time. Callee2559 appears61 times with61 distinct first-argument handles, rising from8401 to18323.
- Top recorded call-site position81026 is span start+1 in script7.
- No prior error or report was observed. No script throw or resource error preceded the fatal.
- All counters were zero at72 task boundaries before the fatal and after unwind. No counter leak was observed.

Not established: whether this is a finite deep walk, a true cycle on the repeated-argument edge, or unbounded regeneration. Distinct fresh arguments on one edge do not prove finiteness. The worker did not inspect child/return relationships far enough to settle this. It did not capture the refused callee.

The latest worker's mapping of script numbers to asset names is unverified. Earlier parent traces verified script7 as fd9d1056-41ce205d8e0c6ad6.js and script8 as2117-562f130d3e4e3a40.js on earlier assembled runs. Do not assume either mapping for this exact run. Resolve it by source bytes and function spans. Do not use the worker's guessed2117 offset as proof.

Worker scratch evidence: /tmp/claude-1000/-home-mrwick--t3-worktrees-bare-t3-e89154d5/89d48733-d597-44d2-853c-e7e2d5d1d9ca/scratchpad/run1.out and run1.err. Only2 fresh runtime processes ran under1GiB/45s. No guard was changed. The exact task category was only narrowed to run_scheduled_tasks, not Deliver/timer specifically.

## Other established findings

A reduced controlled input mounts using the real cached React runtime in bare. Browser::type_text updates native input state without going through the JS value setter. React onChange fires and state-driven text updates for a then ab. A JS-side value write followed by input dispatch is suppressed by React's own tracker. This was verified twice over loopback with only webpack, fd9d1056 and2117 chunks. Temporary probe source was removed. The full login handler, submit, auth and geometry were not tested by that probe.

PR116 keeps genuine platform gates. Native dimensions refuse visible shadow hosts because CSS and layout walk light children only. Walking shadow children alone is wrong: shadow nodes have no computed styles, inheritance would use initial style instead of host style, and there is no scoped shadow sheet or flat-tree/slot model. Hidden shadow hosts are skipped. The source-observed Radix hook reads offsetWidth/offsetHeight, then uses new ResizeObserver and observe with box:border-box. ResizeObserver is absent. Neither it nor shadow geometry is verified as the current runtime blocker.

## Resume order

1. Read this file and git status/log in statement-machine-review. Preserve the explicit hold until Arjun resumes.
2. Resolve the latest active script/function/source mapping and repeated-argument recursion. Do not classify this as a finite fiber walk solely from the prior TraversalDepth diagnosis.
3. Keep all current guards. Do not raise a cap or change accounting to conceal a loop. Any later policy decision needs evidence and separate validation.
4. Complete the parent statement-machine review. The code is local only. Decide its final scope from the diagnosis, then update docs and create a PR only after the hold is lifted.
5. Before a PR, read ~/.agents/docs/pr-writing.md. Push with the repo identity and mrwick1 account, link the PR as the next tool call, then merge only within the existing authorization. Use wt-done only after merge. No PR or watch currently exists for this branch.
6. Continue full cached hydration before live authentication. Read credential and E2E policies before using the vault. The vault reference voicebot/staging-admin has not been used in this work. Never persist credentials or run bundles in Node.

No live staging, credentials, GUI, full browser comparison, successful form hydration or authenticated journey was tested in this pending integration.
