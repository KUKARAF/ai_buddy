// Global open/close state for the "New goal" wizard modal, so any surface
// (top-bar button, banner, empty states, Discover cards) can launch it.

export interface WizardPrefill {
	/** Pre-fill the step-1 "someday" textarea. */
	title?: string;
	/** Pre-select a category in step 2. */
	category?: string;
}

class WizardState {
	open = $state(false);
	prefill = $state<WizardPrefill | null>(null);

	openWizard(prefill?: WizardPrefill): void {
		this.prefill = prefill ?? null;
		this.open = true;
	}

	close(): void {
		this.open = false;
		this.prefill = null;
	}
}

export const wizard = new WizardState();
