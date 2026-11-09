<script setup lang="ts">
/**
 * Визуальный аналог GlobalNotifications.vue сайта: стеклянная плашка,
 * цветная полоса слева, мягкое раскрытие по высоте.
 */
import type { UiNotification, UiNotificationType } from '../types/notifications';

defineProps<{ items: UiNotification[] }>();
const emit = defineEmits<{ dismiss: [id: string] }>();

const tone: Record<UiNotificationType, { bar: string; title: string }> = {
  success: { bar: 'bg-success-500', title: 'text-success-400' },
  error: { bar: 'bg-error-500', title: 'text-error-400' },
  warning: { bar: 'bg-warning-500', title: 'text-warning-400' },
  info: { bar: 'bg-primary-500', title: 'text-primary-400' },
};

function runAction(item: UiNotification) {
  if (item.action) void item.action.onClick();
}
</script>

<template>
  <Teleport to="body">
    <div
      class="fixed left-1/2 top-[4.5rem] z-[10000] w-[30rem] max-w-[calc(100vw-2rem)] -translate-x-1/2 px-4"
      role="status"
      aria-live="polite"
    >
      <TransitionGroup tag="div" name="notif" class="flex flex-col gap-2.5">
        <div v-for="item in items" :key="item.id" class="notif">
          <div
            class="flex overflow-hidden rounded-xl border border-white/10 bg-[#0f1626]/95 shadow-2xl backdrop-blur-xl"
          >
            <div :class="[tone[item.type].bar, 'w-1.5 shrink-0']" />
            <div class="min-w-0 flex-1 p-4">
              <div class="flex items-start justify-between gap-3">
                <div class="min-w-0 flex-1">
                  <div class="flex items-center gap-2">
                    <span
                      :class="[tone[item.type].bar, 'h-2 w-2 shrink-0 rounded-sm']"
                      aria-hidden="true"
                    />
                    <p :class="[tone[item.type].title, 'text-sm font-bold']">{{ item.title }}</p>
                  </div>
                  <p v-if="item.message" class="ml-4 mt-1 text-xs leading-relaxed text-secondary">
                    {{ item.message }}
                  </p>
                  <button
                    v-if="item.action"
                    type="button"
                    :class="[
                      tone[item.type].title,
                      'ml-4 mt-1.5 cursor-pointer text-xs font-medium hover:underline',
                    ]"
                    @click="runAction(item)"
                  >
                    {{ item.action.label }}
                  </button>
                </div>
                <button
                  type="button"
                  class="shrink-0 cursor-pointer rounded p-1 text-tertiary transition-colors hover:text-primary"
                  aria-label="Закрыть уведомление"
                  @click="emit('dismiss', item.id)"
                >
                  <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M6 18 18 6M6 6l12 12" />
                  </svg>
                </button>
              </div>
            </div>
          </div>
        </div>
      </TransitionGroup>
    </div>
  </Teleport>
</template>

<style scoped>
.notif {
  max-height: 12rem;
}

.notif-move,
.notif-enter-active,
.notif-leave-active {
  transition:
    opacity 0.2s ease,
    transform 0.25s ease,
    max-height 0.3s ease;
}

.notif-enter-active,
.notif-leave-active {
  overflow: hidden;
}

.notif-enter-from,
.notif-leave-to {
  max-height: 0;
  opacity: 0;
  transform: translateY(-8px);
}
</style>
