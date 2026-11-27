# Input: the stdout of a Cargo build run with --message-format=json-render-diagnostics.
# Fails when a package Cargo compiled had a feature named test-util enabled, or when no
# build message arrived to check. Lines that are not JSON are skipped.
[inputs | fromjson? | select(.reason? == "compiler-artifact")] as $compiled
| [$compiled[] | select(any(.features[]; . == "test-util")) | .package_id] | unique as $offenders
| if ($compiled | length) == 0 then
    "No Cargo build messages were read, so the features of the shipped build are unchecked.\n" | halt_error(1)
  elif ($offenders | length) > 0 then
    "The shipped build enables a test-util feature in:\n\($offenders | map("  \(.)\n") | add)Only dev-dependencies and the test-support crates may turn one on.\n" | halt_error(1)
  else
    "No test-util feature in \($compiled | map(.package_id) | unique | length) compiled packages."
  end
