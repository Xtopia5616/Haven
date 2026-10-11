import type {
	ImageGenConfigInput,
	SttConfigInput,
	TtsConfigInput,
} from './contracts/generatedCommands.ts';

/** UI drafts use camelCase for the configured connection reference. */
type ProviderConnectionDraft<T extends { provider_name?: string }> = Omit<
	Required<T>,
	'provider_name'
> & { providerName: string };

export type SttConfigDraft = ProviderConnectionDraft<SttConfigInput>;
export type TtsConfigDraft = ProviderConnectionDraft<TtsConfigInput>;
export type ImageGenConfigDraft = ProviderConnectionDraft<ImageGenConfigInput>;
