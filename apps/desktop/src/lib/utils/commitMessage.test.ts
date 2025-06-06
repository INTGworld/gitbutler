import { splitMessage } from '$lib/utils/commitMessage';
import { expect, test, describe } from 'vitest';

describe.concurrent('#splitMessage', () => {
	test('When provided an empty string, it returns an empty title and description', () => {
		expect(splitMessage('')).toMatchObject({ title: '', description: '' });
	});

	test('When provided a single line, it returns a title and empty description', () => {
		const message = 'Fixed all the bugs!';

		expect(splitMessage(message)).toMatchObject({ title: 'Fixed all the bugs!', description: '' });
	});

	test('When provided a single line, --with empty lines following it-- it returns a title and empty description', () => {
		const message = 'Fixed all the bugs!\n\n';

		expect(splitMessage(message)).toMatchObject({ title: 'Fixed all the bugs!', description: '' });
	});

	test('When provided a commit message with one newline, it returns a title and description', () => {
		const message = 'Fixed all the bugs!\nActually maybe not...';

		expect(splitMessage(message)).toMatchObject({
			title: 'Fixed all the bugs!',
			description: 'Actually maybe not...'
		});
	});

	test('Titles are trimmed by default', () => {
		const message = '   Fixed all the bugs!   \nActually maybe not...';
		const title = 'Fixed all the bugs!';
		const description = 'Actually maybe not...';
		expect(splitMessage(message)).toMatchObject({ title, description });
	});

	test('If specified, titles are not trimmed', () => {
		const message = '   Fixed all the bugs!   \nActually maybe not...';
		const title = '   Fixed all the bugs!   ';
		const description = 'Actually maybe not...';
		expect(splitMessage(message, true)).toMatchObject({ title, description });
	});

	test('When provided a commit message with multiple newline, it returns a title and description', () => {
		const message = 'Fixed all the bugs!\n\nActually maybe not...';

		expect(splitMessage(message)).toMatchObject({
			title: 'Fixed all the bugs!',
			description: 'Actually maybe not...'
		});

		const message2 = 'Fixed all the bugs!\n\n\n\nActually maybe not...';

		expect(splitMessage(message2)).toMatchObject({
			title: 'Fixed all the bugs!',
			description: 'Actually maybe not...'
		});
	});

	test('When proivded a commit message with newlines in the description, it returns a title and description', () => {
		const message = `Fixed all the bugs!

Broke something else

Made it better
Got a dog

I fancy coffee`;

		const title = 'Fixed all the bugs!';
		const description = `Broke something else

Made it better
Got a dog

I fancy coffee`;

		expect(splitMessage(message)).toMatchObject({ title, description });
	});

	test('Only the new-lines in the beginning and end of the description are removed', () => {
		const message = `Fixed all the bugs!





Broke something else

Made it better
Got a dog

I fancy coffee   



`;

		const title = 'Fixed all the bugs!';
		const description = `Broke something else

Made it better
Got a dog

I fancy coffee   `;

		expect(splitMessage(message)).toMatchObject({ title, description });
	});
});
