<script setup lang="ts">
import type { ExecutionDto } from "../generated/contracts";
import { time } from "../format";
import StateBadge from "./StateBadge.vue";
defineProps<{ items: ExecutionDto[] }>();
</script>
<template>
  <div class="table-wrap">
    <table>
      <thead>
        <tr>
          <th>Workload / execution</th>
          <th>State</th>
          <th>Started</th>
          <th>Outcome</th>
          <th><span class="sr-only">Inspect execution</span></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="execution in items" :key="execution.attempt_id">
          <td>
            <RouterLink
              :to="`/executions/${encodeURIComponent(execution.execution_id)}`"
              class="strong"
              >{{ execution.name }}</RouterLink
            ><small
              >{{ execution.namespace }} / {{ execution.execution_id }}</small
            >
          </td>
          <td><StateBadge :state="execution.state" /></td>
          <td>{{ time(execution.started_at_us) }}</td>
          <td>
            {{
              execution.error_code ??
              (execution.exit_code !== null
                ? `Exit ${execution.exit_code}`
                : "—")
            }}
          </td>
          <td>
            <RouterLink
              class="text-link"
              :to="`/executions/${encodeURIComponent(execution.execution_id)}`"
              >Inspect <span aria-hidden="true">↗</span></RouterLink
            >
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>
