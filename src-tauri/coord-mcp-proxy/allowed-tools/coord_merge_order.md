`coord_merge_order` stays IN although coord keeps it off the device floor.
It is a READ: it composes a safe merge ordering from the queue, the PR
dependency edges and the plan `Depends-On` graph. coord grants it to the
per-agent `merger` role template, and merger agents reach this door with
their own JWT, so coord's allow-set is the gate for each principal: a device
gets coord's refusal, and a merger gets the tool (the runner's merge-shepherd
playbook names it as one of its merge levers). The reverse-parity
observation reports it for a DEVICE principal, and that report is expected.
It is not the only such name: about fifty forwarded names were off coord's
device floor at `origin/main` `2e9c85585` (for example `coord_claim_acquire`,
`coord_build_info`, `coord_yield`). That backlog is why the observation logs
at INFO and is read from `/health`, not raised as a WARN on every boot.
