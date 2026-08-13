# Trace v1 characterization fixture

`speedtests.jsonl` is an observed prefix of a live legacy-controller capture
recorded during speed and LibreQoS testing. It preserves the original records,
sequence values, and order through evaluation 150. The prefix intentionally has
no synthetic `end` record and may include undesirable legacy behavior; it is an
extraction-characterization artifact, not a correctness oracle.
