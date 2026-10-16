<script setup lang="ts">
withDefaults(
  defineProps<{
    variant?: 'primary' | 'success' | 'secondary' | 'danger';
    size?: 'sm' | 'md' | 'lg';
    disabled?: boolean;
    loading?: boolean;
    fullWidth?: boolean;
  }>(),
  { variant: 'primary', size: 'md', disabled: false, loading: false, fullWidth: false },
);

const styles: Record<string, string> = {
  primary: 'bg-primary-500 hover:bg-primary-600 text-white shadow-md',
  success: 'bg-success-500 hover:bg-success-600 text-white shadow-md',
  secondary:
    'bg-transparent text-primary border border-border-primary hover:border-border-accent hover:bg-surface-light',
  danger: 'bg-error-500 hover:bg-error-600 text-white shadow-md',
};
const sizes: Record<string, string> = {
  sm: 'px-3 py-1.5 text-xs',
  md: 'px-4 py-2.5 text-sm',
  lg: 'px-5 py-3 text-base',
};
</script>

<template>
  <button
    type="button"
    :disabled="disabled || loading"
    :class="[
      'inline-flex items-center justify-center gap-2 rounded-lg font-semibold transition-all cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed',
      styles[variant],
      sizes[size],
      fullWidth ? 'w-full' : '',
    ]"
  >
    <svg
      v-if="loading"
      class="animate-spin h-4 w-4"
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" />
      <path
        class="opacity-75"
        fill="currentColor"
        d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
      />
    </svg>
    <slot />
  </button>
</template>
