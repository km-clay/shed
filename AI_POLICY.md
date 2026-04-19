# `shed` AI usage policy

## Disclosures

Some things need to be made clear regarding AI use in this codebase, as well as setting expectations for AI usage in contributions to it.

I have personally used AI to assist with development in a few areas:
* `help` pages: LLMs are very useful for writing out large amounts of formatted text, and as such are indispensible for creating documentation like this.
* mechanical "janitor work": Stuff like fixing simple bugs, changing symbol names, extracting logic into helper functions, etc.
* UI geometry calculation: Stuff like the fuzzy finder window and prompt layout calculations were written with assistance from AI.
* Summarizing references: I've referenced the codebases for `fish`, `zsh`, and `bash` when making executive design decisions for `shed`. When making these references, I have made use of AI for reading through the existing implementations and summarizing their design strategies.
* Creation of unit tests.
* Writing documentation comments for functions/structs.
* Some parts of the README have been generated.

In all of the above cases, any direct work done by AI has been closely supervised and corrected where wrong.
In general, all of the architectural decisions and implementation details for `shed` have been designed and written by me.

## Acceptable use in contributions

1. Messages in issues or pull requests should be written by you.
2. If you can't explain the code you're submitting, it won't be merged.
3. If the code is incomprehensible, it won't be merged. Even if it "works".
4. In general, you are responsible for the code you are submitting.

I've gone to great lengths to make this codebase a place where humans can work, so please respect that.
