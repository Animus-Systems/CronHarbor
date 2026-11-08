<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { dashboard as d } from "../store";
import { formatError } from "../api";
import { isAlertDelivery, isAlertStatus, pageOf } from "../guards";
import { time } from "../format";
import type { AlertDeliveryDto, AlertStatusDto } from "../generated/contracts";
import ConfirmDialog from "./ConfirmDialog.vue";

const status = ref<AlertStatusDto>();
const deliveries = ref<AlertDeliveryDto[]>([]);
const cursor = ref<string | null>(null);
const nextCursor = ref<string | null>(null);
const error = ref("");
const progress = ref("");
const loading = ref(false);
const selected = ref<AlertDeliveryDto>();
let stopped = false;
let timer: ReturnType<typeof setTimeout> | undefined;
const permitted = computed(() => d.can("operations.manage") && Boolean(d.principal.value?.namespaces.includes("*")));
const enabled = computed(() => status.value !== undefined && status.value.configuration_state !== "disabled" && status.value.configuration_state !== "worker_unavailable");
function retryable(row: AlertDeliveryDto): boolean {
  return enabled.value && ["FAILED", "BLOCKED", "EXPIRED"].includes(row.state) && !d.status.value?.restore_required;
}
async function refresh(): Promise<void> {
  if (loading.value || stopped) return;
  if (timer) { clearTimeout(timer); timer = undefined; }
  if (!permitted.value || !d.connected.value) {
    timer = setTimeout(() => { void refresh(); }, 5000);
    return;
  }
  loading.value = true;
  const session = d.api.value;
  try {
    const suffix = cursor.value ? `&cursor=${encodeURIComponent(cursor.value)}` : "";
    const [summary, page] = await Promise.all([
      d.client().get("/alerts/status", isAlertStatus),
      d.client().get(`/alerts/deliveries?limit=25${suffix}`, pageOf(isAlertDelivery)),
    ]);
    if (stopped || session !== d.api.value) return;
    status.value = summary;
    deliveries.value = page.items;
    nextCursor.value = page.next_cursor;
    error.value = "";
  } catch (caught) {
    if (!stopped && session === d.api.value) error.value = formatError(caught);
  } finally {
    loading.value = false;
    if (!stopped) timer = setTimeout(() => { void refresh(); }, 5000);
  }
}
function next(): void { cursor.value = nextCursor.value; void refresh(); }
function first(): void { cursor.value = null; void refresh(); }
async function retry(): Promise<void> {
  const row = selected.value;
  selected.value = undefined;
  if (!row || !retryable(row) || !permitted.value || !d.canAct.value) return;
  const session = d.api.value;
  const operation = await d.mutate(`/alerts/deliveries/${encodeURIComponent(row.alert_id)}/retry`, {});
  if (stopped || session !== d.api.value) return;
  if (!operation) { error.value = d.error.value || "Retry was not confirmed."; return; }
  progress.value = `Retry operation ${operation.id}: ${operation.state.toLowerCase()}. Check the delivery state for its receiver acknowledgement.`;
  await refresh();
}
onMounted(() => { void refresh(); });
onBeforeUnmount(() => { stopped = true; if (timer) clearTimeout(timer); });
</script>

<template>
  <section v-if="permitted" class="panel">
    <div class="panel-heading">
      <div><h2>Alert deliveries</h2><p>One configured HTTPS destination · metadata only</p></div>
      <button :disabled="loading || !d.connected.value" @click="refresh">Refresh alert deliveries</button>
    </div>
    <p v-if="error" class="message danger" role="alert">{{ error }}</p>
    <p v-if="progress" class="message" role="status">{{ progress }}</p>
    <p v-if="!status" class="inline-empty" role="status">{{ loading ? "Loading alert deliveries…" : "Alert delivery status is unavailable." }}</p>
    <template v-else>
      <p v-if="status.configuration_state === 'disabled'" class="message" role="status">Alerts are disabled in daemon configuration. Retained history remains available.</p>
      <p v-else-if="status.configuration_state !== 'ready'" class="message warning" role="status">Sender unavailable: {{ status.configuration_state.replaceAll('_', ' ') }}. Repair the daemon configuration or credential before an explicit retry.</p>
      <p v-else class="hint">Sender enabled. A delivered alert confirms acceptance by the receiving API. Grokbot processing is tracked by that application.</p>
      <p v-if="status.blocked || status.failed" class="message warning" role="status">Delivery needs review: {{ status.blocked }} blocked and {{ status.failed }} failed in retained history.</p>
      <p v-if="d.status.value?.restore_required" class="message warning" role="status">Restore quarantine holds alert delivery until explicit reconciliation and resume.</p>
      <dl class="detail-grid">
        <div><dt>Pending / in flight</dt><dd>{{ status.pending }} / {{ status.in_flight }}</dd></div>
        <div><dt>Blocked / failed</dt><dd>{{ status.blocked }} / {{ status.failed }}</dd></div>
        <div><dt>Oldest pending</dt><dd>{{ time(status.oldest_pending_at_us) }}</dd></div>
        <div><dt>Last API acknowledgement</dt><dd>{{ time(status.last_acknowledged_at_us) }}</dd></div>
        <div><dt>Queue overflow suppressed</dt><dd>{{ status.queue_overflow_count.toLocaleString() }}</dd></div>
        <div><dt>History evicted / restore suppressed</dt><dd>{{ status.history_eviction_count.toLocaleString() }} / {{ status.restore_suppressed_count.toLocaleString() }}</dd></div>
      </dl>
      <p v-if="status.last_error_code" class="message warning" role="status">Latest sender error: {{ status.last_error_code }}</p>
      <div v-if="deliveries.length" class="table-wrap">
        <table>
          <caption class="sr-only">Bounded alert delivery history</caption>
          <thead><tr><th scope="col">Workload / incident</th><th scope="col">Delivery</th><th scope="col">Attempts / next attempt</th><th scope="col">Action</th></tr></thead>
          <tbody><tr v-for="row in deliveries" :key="row.alert_id">
            <td>{{ row.namespace }}/{{ row.workload_name }}<br /><span class="mono">{{ row.alert_id }}</span><br />{{ row.kind.replaceAll('_', ' ') }} · incident version {{ row.incident_version }}</td>
            <td>{{ row.state }}<br /><span v-if="row.last_error_code">{{ row.last_error_code }}</span><span v-if="row.http_status"> · HTTP {{ row.http_status }}</span></td>
            <td>{{ row.attempt_count }}<br />{{ time(row.next_attempt_at_us) }}</td>
            <td><button v-if="['FAILED', 'BLOCKED', 'EXPIRED'].includes(row.state)" :aria-label="`Review retry for alert ${row.alert_id}`" :disabled="!retryable(row) || !d.canAct.value" @click="selected = row">Review retry</button><span v-else>—</span></td>
          </tr></tbody>
        </table>
      </div>
      <p v-else class="inline-empty">No retained deliveries on this page.</p>
      <div class="row-actions">
        <button v-if="cursor" :disabled="loading" @click="first">First delivery page</button>
        <button v-if="nextCursor" :disabled="loading" @click="next">Next delivery page</button>
      </div>
      <p class="hint">At most 25 rows per page. Outstanding deliveries and terminal history are bounded; suppressed alerts were never delivered. Destination and credential changes require daemon configuration.</p>
    </template>
  </section>
  <ConfirmDialog :open="selected !== undefined" title="Retry one alert delivery" :impact="`Send alert ${selected?.alert_id} once more with its original ID and content. An earlier attempt may already have reached the API. This does not restart the workload or rearm automatic retries.`" confirm-label="Send one retry" @cancel="selected = undefined" @confirm="retry" />
</template>
