<script lang="ts">
	import { MergeMethod } from '$lib/forge/interface/types';
	import { persisted, type Persisted } from '@gitbutler/shared/persisted';
	import ContextMenuItem from '@gitbutler/ui/ContextMenuItem.svelte';
	import ContextMenuSection from '@gitbutler/ui/ContextMenuSection.svelte';
	import DropDownButton from '@gitbutler/ui/DropDownButton.svelte';
	import type { Props as ButtonProps } from '@gitbutler/ui/Button.svelte';

	interface Props {
		projectId: string;
		onclick: (method: MergeMethod) => Promise<void>;
		disabled?: boolean;
		wide?: boolean;
		tooltip?: string;
		style?: ButtonProps['style'];
		kind?: ButtonProps['kind'];
	}

	const {
		projectId,
		onclick,
		disabled = false,
		wide = false,
		tooltip = '',
		style = 'neutral',
		kind = 'outline'
	}: Props = $props();

	function persistedAction(projectId: string): Persisted<MergeMethod> {
		const key = 'projectMergeMethod';
		return persisted<MergeMethod>(MergeMethod.Merge, key + projectId);
	}

	const action = persistedAction(projectId);

	let dropDown: ReturnType<typeof DropDownButton> | undefined;
	let loading = $state(false);

	const labels = {
		[MergeMethod.Merge]: 'Merge pull request',
		[MergeMethod.Rebase]: 'Rebase and merge',
		[MergeMethod.Squash]: 'Squash and merge'
	};
</script>

<DropDownButton
	bind:this={dropDown}
	onclick={async () => {
		loading = true;
		try {
			await onclick?.($action);
		} finally {
			loading = false;
		}
	}}
	{style}
	{kind}
	{loading}
	{wide}
	{tooltip}
	{disabled}
>
	{labels[$action]}
	{#snippet contextMenuSlot()}
		<ContextMenuSection>
			{#each Object.values(MergeMethod) as method}
				<ContextMenuItem
					label={labels[method]}
					onclick={() => {
						$action = method;
						dropDown?.close();
					}}
				/>
			{/each}
		</ContextMenuSection>
	{/snippet}
</DropDownButton>
