<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { dashboard as d } from "../store";
import { formatError } from "../api";
import { isLogStorage } from "../guards";
import type { LogStorageDto, LogStoragePolicyInput } from "../generated/contracts";
import ConfirmDialog from "./ConfirmDialog.vue";

const storage = ref<LogStorageDto>();
const draftRevision = ref(0);
const bytes = ref("");
const segments = ref("");
const retention = ref("");
const error = ref("");
const loading = ref(false);
const proposal = ref<LogStoragePolicyInput>();
const operation = ref("");
let stopped = false;
let timer: ReturnType<typeof setTimeout> | undefined;
const permitted = computed(() => d.can("operations.manage") && Boolean(d.principal.value?.namespaces.includes("*")));
const stale = computed(() => storage.value !== undefined && storage.value.policy.revision !== draftRevision.value);

function adopt(): void {
  if (!storage.value) return;
  draftRevision.value = storage.value.policy.revision;
  bytes.value = String(storage.value.policy.max_disk_bytes);
  segments.value = String(storage.value.policy.max_segments);
  retention.value = String(storage.value.policy.retention_seconds);
  proposal.value = undefined;
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
    const result = await d.client().get("/log-storage", isLogStorage);
    if (stopped || session !== d.api.value) return;
    storage.value = result;
    if (draftRevision.value === 0) adopt();
    error.value = "";
  } catch (caught) {
    if (!stopped && session === d.api.value) error.value = formatError(caught);
  } finally {
    loading.value = false;
    if (!stopped) timer = setTimeout(() => { void refresh(); }, 5000);
  }
}
function prepare(): void {
  error.value = "";
  const policy: LogStoragePolicyInput = {
    max_disk_bytes: Number(bytes.value),
    max_segments: Number(segments.value),
    retention_seconds: Number(retention.value),
  };
  if (!Number.isSafeInteger(policy.max_disk_bytes) || policy.max_disk_bytes < 1 || policy.max_disk_bytes > 10737418240 ||
      !Number.isSafeInteger(policy.max_segments) || policy.max_segments < 1 || policy.max_segments > 100000 ||
      !Number.isSafeInteger(policy.retention_seconds) || policy.retention_seconds < 1 || policy.retention_seconds > 9223372036854) {
    error.value = "Enter whole positive values: up to 10 GiB, 100,000 segments, and a supported retention duration.";
    return;
  }
  if (stale.value) { error.value = "The policy changed. Use the latest policy before reviewing another update."; return; }
  proposal.value = policy;
}
async function save(): Promise<void> {
  const policy = proposal.value;
  proposal.value = undefined;
  if (!policy || !permitted.value || stale.value) return;
  const session = d.api.value;
  const result = await d.mutate("/log-storage", { policy, expected_revision: draftRevision.value });
  if (stopped || session !== d.api.value) return;
  if (!result) { error.value = d.error.value || "Policy update was not confirmed."; return; }
  operation.value = `Policy operation ${result.id}: ${result.state.toLowerCase()}. The installed policy below is authoritative.`;
  if (timer) clearTimeout(timer);
  await refresh();
  if (!stopped && session === d.api.value) adopt();
}
onMounted(() => { void refresh(); });
onBeforeUnmount(() => { stopped = true; if (timer) clearTimeout(timer); });
</script>

<template>
  <section v-if="permitted" class="panel">
    <div class="panel-heading">
      <div><h2>Log storage</h2><p>One installation policy · per-workload capture budgets remain separate</p></div>
      <button :disabled="loading || !d.connected.value" @click="refresh">Refresh log storage</button>
    </div>
    <p v-if="error" class="message danger" role="alert">{{ error }}</p>
    <p v-if="operation" class="message" role="status">{{ operation }}</p>
    <p v-if="!storage" class="inline-empty" role="status">{{ loading ? "Loading log storage…" : "Log storage is unavailable." }}</p>
    <template v-else>
      <p v-if="storage.policy.imported_conservative" class="message warning" role="status">Upgrade imported a conservative log limit from legacy workload settings, including deleted workloads. Review the installation policy.</p>
      <p v-if="storage.usage.under_pressure" class="message warning" role="status">Log storage is under pressure. Protected evidence is retained; capture may stop.</p>
      <dl class="detail-grid">
        <div><dt>Retained bytes / files</dt><dd>{{ storage.usage.retained_bytes.toLocaleString() }} / {{ storage.usage.retained_segments.toLocaleString() }}</dd></div>
        <div><dt>Reserved bytes / files</dt><dd>{{ storage.usage.reserved_bytes.toLocaleString() }} / {{ storage.usage.reserved_segments.toLocaleString() }}</dd></div>
        <div><dt>Protected bytes / files</dt><dd>{{ storage.usage.protected_bytes.toLocaleString() }} / {{ storage.usage.protected_segments.toLocaleString() }}</dd></div>
        <div><dt>Unexplained bytes / files</dt><dd>{{ storage.usage.unexplained_bytes.toLocaleString() }} / {{ storage.usage.unexplained_segments.toLocaleString() }}</dd></div>
        <div><dt>Capture errors</dt><dd>{{ storage.usage.capture_error_count.toLocaleString() }}</dd></div>
        <div><dt>Installed revision</dt><dd>{{ storage.policy.revision }}</dd></div>
      </dl>
      <p v-if="stale" class="message warning" role="status">The installed policy changed. <button @click="adopt">Use latest policy</button></p>
      <form class="panel-content" @submit.prevent="prepare">
        <label class="field">Captured log byte budget<input v-model="bytes" type="number" min="1" max="10737418240" step="1" required /></label>
        <label class="field">Physical segment budget<input v-model="segments" type="number" min="1" max="100000" step="1" required /></label>
        <label class="field">Eligible segment retention (seconds)<input v-model="retention" type="number" min="1" max="9223372036854" step="1" required /></label>
        <p class="hint">Applies across all namespaces. Active and UNKNOWN evidence prevents unsafe reductions. Captured log limits exclude database files, backups and executor-owned logs.</p>
        <button type="submit" :disabled="!d.canAct.value || stale || draftRevision === 0">Review log policy</button>
      </form>
    </template>
  </section>
  <ConfirmDialog :open="proposal !== undefined" title="Update installation log policy" :impact="`Apply ${proposal?.max_disk_bytes.toLocaleString()} bytes, ${proposal?.max_segments.toLocaleString()} segments and ${proposal?.retention_seconds.toLocaleString()} seconds across all namespaces. Protected evidence will not be deleted to make the policy fit.`" confirm-label="Apply log policy" @cancel="proposal = undefined" @confirm="save" />
</template>
