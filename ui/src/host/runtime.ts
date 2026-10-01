type I18nHook = () => { t: (key: string, values?: Record<string, unknown>) => string; locale: string };
type Host = { hooks?: { useI18n?: I18nHook } };

declare const __POM_PLUGIN_CODE__: string;

export function usePluginI18n(): ReturnType<I18nHook> {
  const host = (globalThis as typeof globalThis & { __POM_HOST__?: Host }).__POM_HOST__;
  if (!host?.hooks?.useI18n) throw new Error("POM host i18n is unavailable");
  const { t, locale } = host.hooks.useI18n();
  return { t: (key, values) => t(`${__POM_PLUGIN_CODE__}.${key}`, values), locale };
}
