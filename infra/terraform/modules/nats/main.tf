terraform {
  required_version = ">= 1.4"
}

# ---------------------------------------------------------------------------
# KV buckets — mirrors crates/dispatcher/src/nats_init.rs
#
# Bucket names from crates/types/src/lib.rs (buckets module).
# The dispatcher's create_key_value is idempotent (create-or-update),
# so pre-creating these is safe.
# ---------------------------------------------------------------------------

locals {
  server = "-s ${var.nats_url}"

  kv_buckets = {
    chuggernaut_jobs       = {}
    chuggernaut_claims     = {}
    chuggernaut_deps       = {}
    chuggernaut_counters   = {}
    chuggernaut_activities = {}
    chuggernaut_journal    = { ttl = "7d" }
    chuggernaut_channels   = {}
  }
}

resource "terraform_data" "kv_buckets" {
  for_each = local.kv_buckets

  input = each.key

  provisioner "local-exec" {
    command = <<-EOT
      set -euo pipefail
      BUCKET="${each.key}"
      TTL_FLAG="${lookup(each.value, "ttl", "") != "" ? "--ttl=${each.value.ttl}" : ""}"

      # Idempotent: check if exists, create if not
      if nats kv info "$BUCKET" ${local.server} >/dev/null 2>&1; then
        echo "KV bucket $BUCKET: already exists"
      else
        nats kv add "$BUCKET" --history=1 --storage=file $TTL_FLAG ${local.server}
        echo "KV bucket $BUCKET: created"
      fi
    EOT
  }
}

# ---------------------------------------------------------------------------
# JetStream streams
#
# NOT managed by Terraform — the dispatcher creates these on startup with
# exact config via create_stream() which is idempotent. The nats CLI
# --defaults flag sets options that conflict with the Rust defaults, so
# letting the dispatcher be the single authority avoids config drift.
# ---------------------------------------------------------------------------
