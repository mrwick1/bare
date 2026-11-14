# Fetch Metadata pre-merge checks

Date: 2026-11-14. Frozen main: 5bc18d14.

The existing approved Fetch Metadata implementation was rebased onto current main. Header values and policy are unchanged. The first serial release run passed 1,461 tests and failed one stylesheet-replacement test. That test then passed in isolation on both candidate and unchanged main. The full candidate rerun passed 1,462 tests with six ignored. No cause is assigned to the initial failure.

## Request allocation refinement

The first five-row allocator gate failed on the existing Fetch POST workload: +2.684 percent under glibc and +2.032 percent under jemalloc. Other selected rows passed. The workload recorded 11,033 allocations for 100 requests, against 10,833 on main.

The wire-header vector previously reserved only author-header space, then grew for the generated headers. It now reserves the exact known author, Origin and metadata capacity once. Header validation, order, classification, redirects and CORS rules stay unchanged. The workload returns to 10,833 allocations. All 53 Fetch/XHR release tests pass after the refinement.

## Repeated resource checks

Machine: Linux x86-64, Ryzen 9 5900HX. Two build jobs per build. The host is shared. No wall-time claim is made. Each instruction and peak-RSS interval contains three fresh process runs. Instructions use perf instructions:u and the existing tcache setting. RSS uses KiB and a C fork/exec/wait4 wrapper without that setting. Raw evidence and binary hashes remain private.

| Case | Main instructions | First candidate instructions | Reserved candidate instructions | Main RSS KiB | Reserved RSS KiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| style_setget | 56,310,566–56,326,469 | 56,332,916–56,367,917 | 56,335,938–56,390,734 | 12,364–12,428 | 12,504–12,596 |
| sheets_linked | 8,622,008–8,623,555 | 8,623,863–8,625,807 | 8,624,437–8,624,657 | 12,676–12,856 | 12,616–12,768 |
| browser_mutation | 70,097,876–70,098,479 | 70,100,016–70,101,301 | 70,102,170–70,102,225 | 11,472–12,036 | 11,816–12,036 |
| fetch_post | 21,591,413–21,592,372 | 22,180,283–22,180,806 | 22,060,056–22,064,999 | 13,004–13,280 | 13,000–13,424 |
| xhr | 10,691,794–10,694,630 | 10,702,012–10,702,461 | 10,701,414–10,702,521 | 11,608–11,872 | 11,668–11,956 |

The reserved candidate still costs more instructions on Fetch POST than main. Its final two-allocator gate fails: Fetch POST grows by 2.146 percent under glibc and 1.624 percent under jemalloc. All other selected rows pass. On 2026-11-14 the owner explicitly approved the documented Fetch POST cost exception for this PR. The general 0.5 percent rule remains unchanged. The full repository cost matrix was not run. The earlier accepted Deck journey uses the combined metadata and textarea candidate and remains separate evidence.

## Approved final rebase

The branch was rebased onto main at c4e32a85, which includes the independent textarea fix. The baseline executables have the same production source as that main commit; the production diff was empty. The same five cases were measured again with three instruction and three RSS samples per case. The machine and counter settings are unchanged.

| Case | Main instructions | Candidate instructions | Main RSS KiB | Candidate RSS KiB |
| --- | ---: | ---: | ---: | ---: |
| style_setget | 56,307,038–56,325,007 | 56,316,264–56,344,979 | 12,364–12,508 | 12,288–12,552 |
| sheets_linked | 8,622,191–8,623,830 | 8,624,384–8,625,591 | 12,596–12,832 | 12,596–12,944 |
| browser_mutation | 70,098,558–70,099,172 | 70,100,992–70,102,589 | 11,444–11,908 | 11,424–11,872 |
| fetch_post | 21,587,543–21,592,335 | 22,064,482–22,065,847 | 12,892–13,228 | 13,040–13,104 |
| xhr | 10,690,244–10,695,100 | 10,702,023–10,702,577 | 11,496–11,800 | 11,560–11,972 |

Fetch POST remains within the documented cost accepted by the owner. Other sampled instruction rows remain below the general limit. The earlier two-allocator failure is retained and covered by the scoped exception. A new jemalloc comparison and full matrix were not repeated after this rebase.

The final rebased serial release library run passes 1,464 tests with six ignored. Main remained at c4e32a85 at the final check. The approved exception and original failures remain in this report; the cost gate is not relabeled as an unqualified pass.

## Latest-main integration

Main advanced to 2612b710 before merge. Its new code changes picture loading, browser-session settling and the replay driver. The Fetch Metadata rebase was clean. The same resource cases were rebuilt and measured again on that main and candidate. The three-run method and machine are unchanged.

| Case | Main instructions | Candidate instructions | Main RSS KiB | Candidate RSS KiB |
| --- | ---: | ---: | ---: | ---: |
| style_setget | 56,312,619–56,324,227 | 56,334,676–56,353,194 | 12,200–12,508 | 12,356–12,528 |
| sheets_linked | 8,621,355–8,629,372 | 8,618,447–8,625,616 | 12,548–12,788 | 12,556–13,232 |
| browser_mutation | 70,100,798–70,101,826 | 70,101,245–70,101,696 | 11,544–11,708 | 11,636–11,920 |
| fetch_post | 21,594,146–21,594,950 | 22,063,556–22,065,012 | 12,928–13,100 | 13,016–13,160 |
| xhr | 10,696,656–10,697,623 | 10,700,160–10,703,983 | 11,548–11,620 | 11,832–11,992 |

Fetch POST remains within the scoped owner-approved exception. Other sampled rows remain below the general limit. The 1,464-test full release result above belongs to the preceding c4e32a85 rebase; it is not relabeled as a full-suite result for this head. No new jemalloc comparison, full matrix or UI journey is claimed on this final integration.

All 53 Fetch/XHR release tests pass on the latest picture-loading integration. This focused result is separate from the preceding full-library run.
