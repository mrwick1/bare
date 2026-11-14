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

The reserved candidate still costs more instructions on Fetch POST than main. Its final two-allocator gate fails: Fetch POST grows by 2.146 percent under glibc and 1.624 percent under jemalloc. All other selected rows pass. No threshold change or cost exception is approved. This PR remains unmerged. The full repository cost matrix was not run. The earlier accepted Deck journey uses the combined metadata and textarea candidate and remains separate evidence.
