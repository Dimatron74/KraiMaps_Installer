<!--
  Жёсткий блок устаревшей версии (hard): полноэкранный экран без обхода.
  Показывается только по свежему явному сигналу сервера в этой сессии.
  Запасной выход — ручное скачивание со страницы `/installer` (пользователь
  никогда не застревает: даже если автообновление сломано, есть путь).
-->
<script setup lang="ts">
import { computed } from 'vue';
import { useUpdaterStore } from '../../app/stores/updater';
import Alert from '../../shared/ui/Alert.vue';
import BrandMark from '../../shared/ui/BrandMark.vue';
import Btn from '../../shared/ui/Btn.vue';
import ErrorDetails from '../../shared/ui/ErrorDetails.vue';
import ProgressBar from '../../shared/ui/ProgressBar.vue';

const updater = useUpdaterStore();

const busy = computed(() => updater.phase === 'checking' || updater.phase === 'downloading');
</script>

<template>
  <div
    class="fixed inset-0 z-50 flex items-center justify-center overflow-y-auto bg-surface p-6"
    role="alertdialog"
    aria-modal="true"
    aria-label="Требуется обновление"
  >
    <div class="w-full max-w-md text-center">
      <BrandMark size="lg" class="mx-auto" />
      <h1 class="mt-4 text-2xl font-extrabold text-primary">Нужно обновиться</h1>
      <p class="mx-auto mt-2 max-w-sm text-sm leading-relaxed text-secondary">
        Версия {{ updater.version }} больше не поддерживается{{
          updater.minSupported ? ` (минимум — ${updater.minSupported})` : ''
        }}.
        Обновитесь, чтобы продолжить: это займёт меньше минуты.
      </p>

      <div v-if="updater.notes" class="glass-card mt-4 p-4 text-left">
        <p class="max-h-40 overflow-y-auto whitespace-pre-line text-sm leading-relaxed text-secondary">
          {{ updater.notes }}
        </p>
      </div>

      <div v-if="updater.phase === 'downloading'" class="mt-4 text-left">
        <ProgressBar :percent="updater.percent" label="Скачиваем обновление…" />
      </div>

      <Alert v-if="updater.phase === 'error' && updater.error" tone="error" class="mt-4 text-left">
        <p class="text-sm font-semibold text-primary">{{ updater.error.message }}</p>
        <ErrorDetails
          class="mt-2"
          :code="updater.error.code"
          :message="updater.error.message"
          :details="updater.error.details"
          hint="Автообновление не сработало — скачайте установщик вручную кнопкой ниже, это тот же официальный файл."
        />
      </Alert>

      <div class="mt-5 flex flex-col gap-3">
        <!-- latest=null (битый ответ сервера: блок есть, сборки нет) —
          основная кнопка ведёт на ручное скачивание, а не вхолостую в апдейтер. -->
        <Btn
          v-if="updater.latest"
          size="lg"
          full-width
          :loading="busy"
          :disabled="busy"
          @click="updater.startUpdate()"
        >
          {{ updater.phase === 'downloading' ? 'Обновляем…' : `Обновить до ${updater.latest}` }}
        </Btn>
        <Btn size="lg" full-width :variant="updater.latest ? 'secondary' : 'primary'" :disabled="busy" @click="updater.openManualDownload()">
          Скачать вручную с сайта
        </Btn>
      </div>
      <p v-if="updater.manualMessage" class="mt-3 text-xs text-tertiary">
        {{ updater.manualMessage }}
      </p>
    </div>
  </div>
</template>
