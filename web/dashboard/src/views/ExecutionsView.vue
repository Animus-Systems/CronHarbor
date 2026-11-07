<script setup lang="ts">
import { computed, ref } from "vue";
import { dashboard as d } from "../store";
import ExecutionTable from "../components/ExecutionTable.vue";
defineProps<{ logsMode?: boolean }>();
const query = ref("");
const state = ref("");
const states = computed(() =>
  [...new Set(d.executions.value.map((e) => e.state))].sort(),
);
const items = computed(() =>
  d.executions.value.filter(
    (e) =>
      (!query.value ||
        `${e.name} ${e.execution_id} ${e.namespace}`
          .toLowerCase()
          .includes(query.value.toLowerCase())) &&
      (!state.value || e.state === state.value),
  ),
);
</script>
<template>
  <section class="page-heading">
    <div>
      <p class="eyebrow">EXECUTION EVIDENCE</p>
      <h1>
        {{ logsMode ? "Logs" : "Executions" }}<span class="heading-dot">.</span>
      </h1>
      <p class="muted">
        {{
          logsMode
            ? "Choose an execution to inspect its bounded, untrusted output."
            : "Immutable attempts, observed outcomes, and explicit uncertainty."
        }}
      </p>
    </div>
  </section>
  <p v-if="!d.can('execution.read')" class="message warning">
    Execution visibility requires execution.read. No execution evidence is
    loaded for this token.
  </p>
  <section v-else class="panel">
    <div class="filter-bar">
      <label class="search-field"
        ><svg viewBox="0 0 24 24" aria-hidden="true">
          <circle cx="10" cy="10" r="6" />
          <path d="m15 15 6 6" /></svg
        ><span class="sr-only">Search executions</span
        ><input
          v-model="query"
          type="search"
          placeholder="Search workload, namespace, or execution…" /></label
      ><label
        ><span class="sr-only">Execution state</span
        ><select v-model="state">
          <option value="">All states</option>
          <option v-for="s in states" :key="s">{{ s }}</option>
        </select></label
      ><span class="muted result-count">{{ items.length }} attempts</span>
    </div>
    <ExecutionTable v-if="items.length" :items="items" />
    <div v-else class="empty-state">
      <h2>
        {{
          d.executions.value.length
            ? "No matching executions"
            : "No executions recorded"
        }}
      </h2>
      <p>
        A configured revision and an execution are separate records. Run an
        approved job or wait for its next occurrence.
      </p>
      <RouterLink to="/jobs" class="button">View jobs</RouterLink>
    </div>
    <div v-if="d.executionCursor.value" class="panel-bottom">
      <button @click="d.loadMore('executions')">Load next page</button>
      <p class="hint">Filters apply to loaded pages.</p>
    </div>
  </section>
</template>
