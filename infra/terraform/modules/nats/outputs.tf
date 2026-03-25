output "kv_buckets" {
  value = [
    "chuggernaut_jobs",
    "chuggernaut_claims",
    "chuggernaut_deps",
    "chuggernaut_counters",
    "chuggernaut_activities",
    "chuggernaut_journal",
    "chuggernaut_channels",
  ]
}

output "streams" {
  value = [
    "CHUGGERNAUT-TRANSITIONS",
    "CHUGGERNAUT-MONITOR",
  ]
}
