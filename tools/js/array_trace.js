// The array workloads of `crates/vex/examples/object_census.rs array` (plan step S1.9), as a script for the old engine.
// Prefix it with `var SCALE = N; var KIND = "small";` (KIND is "small", "ints" or "objects"; N = 0 makes the empty baseline).
// "small": N arrays `[i, i + 1, i + 2]` in one array. "ints": N integers pushed to one array. "objects": N empty objects in one array.
var keep = [];
for (var i = 0; i < SCALE; i++) {
  if (KIND === "small") keep.push([i, i + 1, i + 2]);
  else if (KIND === "ints") keep.push(i);
  else keep.push({});
}
keep.length;
