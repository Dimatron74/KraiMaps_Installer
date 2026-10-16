<script setup lang="ts">
import { useInstallStore } from '../../../app/stores/install';
import Alert from '../../../shared/ui/Alert.vue';
import Btn from '../../../shared/ui/Btn.vue';

const store = useInstallStore();
</script>

<template>
  <div>
    <h2 class="text-lg font-bold text-primary">Мир с таким именем уже есть</h2>
    <Alert tone="info" class="mt-3">
      <p class="text-xs">
        Ваш прогресс неприкосновенен: заменять существующий мир запрещено.
        Карта встанет рядом, как отдельный мир.
      </p>
    </Alert>

    <div class="glass-card mt-4 p-4">
      <label class="text-xs font-semibold text-tertiary uppercase tracking-wide" for="world-name">
        Имя нового мира
      </label>
      <input
        id="world-name"
        v-model="store.customWorldName"
        type="text"
        maxlength="64"
        class="mt-2 w-full rounded-lg border border-border-secondary bg-black/30 px-3 py-2 text-sm text-primary outline-none focus:border-border-accent"
      />
    </div>

    <div class="mt-5 flex flex-col gap-2">
      <Btn variant="success" :loading="store.busy" @click="store.confirmConflictName()">
        Установить как «{{ store.customWorldName }}»
      </Btn>
      <Btn variant="secondary" @click="store.step = 'target'">Отмена</Btn>
    </div>
  </div>
</template>
