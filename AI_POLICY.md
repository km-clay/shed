# `shed` AI usage policy

## Disclosures

Regarding AI use in this codebase:

I have personally used AI to assist with development in a few areas:
* `help` pages: LLMs are very useful for writing out large amounts of formatted text, and as such are indispensible for creating documentation like this.
* mechanical "janitor work": Stuff like fixing simple bugs, changing symbol names, extracting logic into helper functions, etc.
* UI geometry calculation: Stuff like the fuzzy finder window and prompt layout calculations were written with assistance from AI.
* Summarizing references: I've referenced the codebases for `fish`, `zsh`, and `bash` when making executive design decisions for `shed`. When making these references, I have made use of AI for reading through the existing implementations and summarizing their design strategies.
* Creation of unit tests.
* Writing documentation comments for functions/structs.

In all of the above cases, any direct work done by AI has been closely supervised and corrected where wrong.
In general, all of the architectural decisions and implementation details for `shed` have been designed and written by me.

## Acceptable use in contributions

Basically all I really care about is that you write the PRs yourself. The code can come from an agent if you wish, with one exception written below. My only real request for most PRs is that you just keep the AI comments to a minimum.

Exception: PRs with large architectural changes or entire new features. Anything that has direct implications for the system and its interface as a whole needs to bo either closely supervised by a human, or ideally, written by human hands.

I've gone to great lengths to make this codebase a place where people can work, so please respect that.
