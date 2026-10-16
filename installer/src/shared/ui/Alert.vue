<script setup lang="ts">
defineProps<{ tone?: 'info' | 'success' | 'warning' | 'error' }>();
const tones: Record<string, string> = {
  info: 'border-primary-500/30 bg-primary-500/10',
  success: 'border-success-500/30 bg-success-500/10',
  warning: 'border-warning-500/30 bg-warning-500/10',
  error: 'border-error-500/30 bg-error-500/10',
};
</script>

<template>
  <div :class="['rounded-xl border p-4 text-sm', tones[$props.tone ?? 'info']]">
    <slot />
  </div>
</template>
