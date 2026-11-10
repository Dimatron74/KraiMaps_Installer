<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue';

const props = withDefaults(
  defineProps<{
    labelledBy: string;
    closeOnBackdrop?: boolean;
  }>(),
  { closeOnBackdrop: true },
);
const emit = defineEmits<{ close: [] }>();

const dialog = ref<HTMLDialogElement | null>(null);

onMounted(() => {
  if (!dialog.value?.open) dialog.value?.showModal();
});

onBeforeUnmount(() => {
  if (dialog.value?.open) dialog.value.close();
});

/** Нативное закрытие по Esc не должно расходиться с состоянием снаружи. */
function onCancel(event: Event) {
  event.preventDefault();
  emit('close');
}

function onBackdropClick(event: MouseEvent) {
  if (!props.closeOnBackdrop) return;
  if (event.target === dialog.value) emit('close');
}
</script>

<template>
  <dialog
    ref="dialog"
    :aria-labelledby="labelledBy"
    class="m-auto max-h-[calc(100vh-2rem)] w-[min(92vw,28rem)] max-w-none overflow-y-auto rounded-2xl border-0 bg-transparent p-0 text-primary [&::backdrop]:bg-black/70 [&::backdrop]:backdrop-blur-sm"
    @cancel="onCancel"
    @click="onBackdropClick"
  >
    <div class="surface-glass rounded-2xl p-6 shadow-[0_24px_80px_rgba(0,0,0,0.55)]">
      <slot />
    </div>
  </dialog>
</template>

<style scoped>
dialog[open] {
  animation: dialog-in 160ms ease-out;
}

@keyframes dialog-in {
  from {
    opacity: 0;
    transform: translateY(8px) scale(0.98);
  }
  to {
    opacity: 1;
    transform: none;
  }
}

@media (prefers-reduced-motion: reduce) {
  dialog[open] {
    animation: none;
  }
}
</style>
