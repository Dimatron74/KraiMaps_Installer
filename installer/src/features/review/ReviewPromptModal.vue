<script setup lang="ts">
import { onMounted } from 'vue';
import Btn from '../../shared/ui/Btn.vue';
import Modal from '../../shared/ui/Modal.vue';
import type { InstalledMap } from '../../shared/utils/reviewPrompt';

defineProps<{
  map: InstalledMap;
  installInProgress: boolean;
}>();

const emit = defineEmits<{ shown: []; rate: []; dismiss: [] }>();

onMounted(() => emit('shown'));
</script>

<template>
  <Modal labelled-by="review-prompt-title" @close="emit('dismiss')">
    <div class="text-center">
      <div
        class="mx-auto flex h-14 w-14 items-center justify-center rounded-2xl border border-emerald-300/30 bg-emerald-400/10 shadow-[0_0_36px_rgba(74,222,128,0.28)]"
      >
        <svg
          class="h-7 w-7 text-emerald-300"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.6"
          aria-hidden="true"
        >
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            d="M11.48 3.5a.562.562 0 0 1 1.04 0l2.125 5.111a.563.563 0 0 0 .475.345l5.518.442c.499.04.701.663.321.988l-4.204 3.602a.563.563 0 0 0-.182.557l1.285 5.385a.562.562 0 0 1-.84.61l-4.725-2.885a.562.562 0 0 0-.586 0L6.982 20.54a.562.562 0 0 1-.84-.61l1.285-5.386a.562.562 0 0 0-.182-.557l-4.204-3.602c-.38-.325-.178-.948.32-.988l5.519-.442a.563.563 0 0 0 .475-.345L11.48 3.5Z"
          />
        </svg>
      </div>

      <p class="mt-4 text-[11px] font-semibold uppercase tracking-[0.2em] text-emerald-300/80">
        Помоги другим игрокам
      </p>
      <h2 id="review-prompt-title" class="mt-2 text-lg font-bold text-primary">
        Оцени скачанную ранее карту «{{ map.title }}»
      </h2>
      <p class="mt-2 text-sm leading-relaxed text-secondary">
        Если уже поиграл, то оставь отзыв на странице карты.
        Это помогает другим игрокам выбирать карты, а авторам делать их лучше.
      </p>
      <p v-if="installInProgress" class="mt-3 text-xs text-tertiary">
        Установка текущей карты продолжается в фоне...
      </p>

      <div class="mt-6 flex flex-col gap-2">
        <Btn size="lg" full-width @click="emit('rate')">
          <svg
            class="h-4 w-4"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            aria-hidden="true"
          >
            <path
              stroke-linecap="round"
              stroke-linejoin="round"
              d="M13.5 6H5.25A2.25 2.25 0 0 0 3 8.25v10.5A2.25 2.25 0 0 0 5.25 21h10.5A2.25 2.25 0 0 0 18 18.75V10.5m-10.5 6L21 3m0 0h-5.25M21 3v5.25"
            />
          </svg>
          Оценить
        </Btn>
        <Btn variant="secondary" full-width @click="emit('dismiss')">Позже</Btn>
      </div>
    </div>
  </Modal>
</template>
