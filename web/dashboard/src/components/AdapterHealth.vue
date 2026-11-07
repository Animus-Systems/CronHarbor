<script setup lang="ts">
import { dashboard as d } from "../store";
import { time } from "../format";
import StateBadge from "./StateBadge.vue";
</script>
<template>
  <section class="panel">
    <div class="panel-heading">
      <div>
        <h2>Executor observations</h2>
        <p>Enabled configuration is separate from verified connectivity.</p>
      </div>
    </div>
    <div v-if="!d.status.value?.adapter_health.length" class="inline-empty">
      No adapter observations are available. Connectivity is unverified.
    </div>
    <div v-else class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Adapter</th>
            <th>Configuration</th>
            <th>Observed state</th>
            <th>Last observation · local</th>
            <th>Evidence</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="adapter in d.status.value.adapter_health"
            :key="adapter.adapter"
          >
            <td class="strong">{{ adapter.adapter }}</td>
            <td>{{ adapter.enabled ? "Enabled" : "Disabled" }}</td>
            <td><StateBadge :state="adapter.state" /></td>
            <td>
              {{
                adapter.last_observed_at_us === null
                  ? "Unverified"
                  : time(adapter.last_observed_at_us)
              }}
            </td>
            <td class="adapter-detail">{{ adapter.detail }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>
