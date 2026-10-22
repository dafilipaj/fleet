All records here are synthetic. No captured prompts, identifiers or machine details are used.

`registry-2.1.300.json` and `registry-2.1.301.json` pin the version gate with a process start
of 2026-01-01 00:00 UTC. Tests copy the sidecars and set deterministic mtimes: stale is one
second before that start, prior and running are 30 seconds after, and the others 120 seconds
after. The Turn starts at 00:01. The running task has no transcript: a quiet task still runs.

The sidecars cover running, finished, stopped, nested, forked, stale, malformed, an earlier
finish, and a task spanning Turns. First timestamps model task starts; filesystem birth
times are deliberately unused. The stopped sidecar has `finished: true` just like a success.

Main Transcripts cover typed, absorbed mid-turn, SDK and skill prompts, and a prompt followed
by task notifications (both forms), local effort entries, a peer message, tool results,
compaction, interruption and bookkeeping. Tests also generate large tails to cover widening
and the 8 MiB limit.

`lifecycle/` pins runs that ended without their task ending: a Transcript with task
notifications in both forms and both content shapes (interim, settled, pending input, notes
quoted only inside `<summary>`/`<result>`, a routing notice without a status) and successful
and failed `TaskStop` results, beside sidecars that all carry `finished: true` except their
owner's. The owner's own log carries its nested tasks' notifications, one later than the
parent's for each. Tests set every sidecar's mtime 120 seconds after the process start.

`codex/` pins a parent and three child rollouts: running, finished in this Turn and finished
in the preceding Turn. They verify that Codex remains unknown until counting and its
process/child-rollout pairing bug are addressed; pi is also outside the counting scope.
