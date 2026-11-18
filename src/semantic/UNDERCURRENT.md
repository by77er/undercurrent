This directory judges semantic rules by calling a model. The model reads
code written by the author of the pull request, who may try to steer it.

## A model's output is reported only after it is checked

Nothing a model returns reaches the report directly.

Text written by a generative model is kept only when its quoted evidence is
found on a changed line of a changed file, and the line number reported is
the one where the evidence was found, not the one the model claimed.

A line picked by a decision model is reported only if it is a changed line.
Without a second reviewer, its verdict blocks only when the probability
clears the blocking threshold; anything lower is printed as a note.

## The model reads only files that belong to the repository

Every path a model asks for goes through `Workspace`, which refuses
anything not in the repository's file list or resolving outside its root.
New tools read through `Workspace` as well; they never open a path taken
from model output themselves.

## Prompts mark reviewed material as data

Any prompt that includes file contents, diffs or tool results tells the
model that this material is under review and is not an instruction.
