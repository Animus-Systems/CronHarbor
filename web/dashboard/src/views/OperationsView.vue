<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import { dashboard as d } from "../store";
import { formatError } from "../api";
import { isOperation } from "../guards";
import { operationFinished, operationSucceeded } from "../operations";
import { time } from "../format";
import StateBadge from "../components/StateBadge.vue";
const route = useRoute();
const router = useRouter();
const lookup = ref(String(route.params.id ?? ""));
const error = ref("");
const busy = ref(false);
let stopped = false;
const selected = computed(() =>
  d.operations.value.find(
    (op) => op.id === String(route.params.id ?? lookup.value),
  ),
);
async function inspect(): Promise<void> {
  const current = d.api.value;
  if (busy.value || !current || !lookup.value.trim()) return;
  const path = route.path;
  const ownsRequest = () =>
    !stopped && d.api.value === current && route.path === path;
  busy.value = true;
  error.value = "";
  try {
    const result = await current.get(
      `/operations/${encodeURIComponent(lookup.value.trim())}`,
      isOperation,
    );
    if (!ownsRequest()) return;
    d.rememberOperation(result);
    if (String(route.params.id ?? "") !== result.id)
      void router.push(`/operations/${encodeURIComponent(result.id)}`);
  } catch (caught) {
    if (ownsRequest()) error.value = formatError(caught);
  } finally {
    if (ownsRequest()) busy.value = false;
  }
}
onMounted(() => {
  if (lookup.value) void inspect();
});
onBeforeUnmount(() => {
  stopped = true;
});
</script>
<template>
  <section class="page-heading">
    <div>
      <p class="eyebrow">DURABLE REQUESTS</p>
      <h1>Operations<span class="heading-dot">.</span></h1>
      <p class="muted">
        Acceptance is a receipt. A final state is the outcome.
      </p>
    </div>
    <span class="badge" :class="d.connected.value ? 'success' : 'warning'">{{
      d.connected.value
        ? "Polling known operations"
        : "Operation state unknown to this tab"
    }}</span>
  </section>
  <section class="panel">
    <form class="filter-bar" @submit.prevent="inspect">
      <label class="field grow"
        >Inspect durable operation ID<input
          v-model="lookup"
          placeholder="Paste an operation ID from the CLI, API, or previous session"
          maxlength="256" /></label
      ><button
        class="primary"
        :disabled="busy || !lookup.trim() || !d.connected.value"
      >
        {{ busy ? "Looking up…" : "Inspect operation" }}
      </button>
    </form>
    <p class="panel-content hint">
      This tab tracks up to 100 known operations in memory. After a reload,
      inspect a durable ID; the daemon retains its authoritative outcome.
    </p>
    <p v-if="error" class="message danger" role="alert">{{ error }}</p>
  </section>
  <section v-if="selected" class="panel">
    <div class="panel-heading">
      <div>
        <h2>Operation detail</h2>
        <p class="mono wrap">{{ selected.id }}</p>
      </div>
      <StateBadge :state="selected.state" />
    </div>
    <dl class="detail-grid">
      <div>
        <dt>Intent</dt>
        <dd>{{ selected.kind }}</dd>
      </div>
      <div>
        <dt>Target</dt>
        <dd class="mono wrap">{{ selected.target_id }}</dd>
      </div>
      <div>
        <dt>Accepted at · UTC</dt>
        <dd>{{ time(selected.created_at_us, true) }}</dd>
      </div>
      <div>
        <dt>Error code</dt>
        <dd>{{ selected.error_code ?? "None recorded" }}</dd>
      </div>
    </dl>
    <div class="panel-content">
      <p v-if="!d.connected.value" class="warning-text">
        The connection is unavailable. The operation continues independently;
        its current state is unknown to this tab. Reconnect to requery the same
        ID.
      </p>
      <p v-else-if="!operationFinished(selected.state)" class="muted">
        The request has not reached a final outcome. Keep this ID; the dashboard
        queries it without resubmitting the mutation.
      </p>
      <p
        v-else
        :class="
          operationSucceeded(selected.state) ? 'success-text' : 'warning-text'
        "
      >
        {{
          operationSucceeded(selected.state)
            ? "The requested operation completed. Inspect workload or execution evidence to verify the operational result."
            : "The operation ended without success. Inspect the error code and current workload state before a new action."
        }}
      </p>
      <RouterLink class="button" to="/jobs">Inspect workloads</RouterLink
      ><RouterLink class="button" to="/executions"
        >Inspect executions</RouterLink
      >
    </div>
  </section>
  <section class="panel">
    <div class="panel-heading">
      <h2>
        Known operations
        <span class="count">{{ d.operations.value.length }}</span>
      </h2>
    </div>
    <div v-if="!d.operations.value.length" class="empty-state">
      <h2>No operations in this tab</h2>
      <p>
        Run an approved workload or inspect a durable operation ID. A
        disconnected client never automatically repeats a mutation.
      </p>
    </div>
    <div v-else class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Operation</th>
            <th>Intent / target</th>
            <th>State</th>
            <th>Accepted · local</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="operation in d.operations.value" :key="operation.id">
            <td>
              <RouterLink
                class="mono"
                :to="`/operations/${encodeURIComponent(operation.id)}`"
                >{{ operation.id }}</RouterLink
              >
            </td>
            <td>
              {{ operation.kind
              }}<small class="mono">{{ operation.target_id }}</small>
            </td>
            <td><StateBadge :state="operation.state" /></td>
            <td>{{ time(operation.created_at_us) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
  <section class="panel">
    <div class="panel-heading">
      <div>
        <h2>Recent live evidence</h2>
        <p>
          Bounded to the latest 100 received events · authoritative data
          refreshed after changes
        </p>
      </div>
    </div>
    <div v-if="!d.events.value.length" class="inline-empty">
      No live events received during this connection.
    </div>
    <div v-else class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Sequence</th>
            <th>Event</th>
            <th>Target</th>
            <th>Time · local</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="event in d.events.value.slice(0, 25)" :key="event.seq">
            <td class="mono">{{ event.seq }}</td>
            <td>{{ event.event_kind }}</td>
            <td class="mono">
              {{ event.execution_id ?? event.workload_id ?? "Instance" }}
            </td>
            <td>{{ time(event.created_at_us) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>
