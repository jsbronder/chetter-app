Chetter monitors pull request events sent from Github and adds some quality of
live improvements intended to assist code review.

# Chetter References
A conscientiousness author might explicitly publish any changes they push to a
pull request branch that is still under review.  Likewise, a diligent reviewer
might create local branch/tag pointing to the tip of the branch they reviewed
so that it could be compared against later pull request updates.  However,
while we all wish each other the best, this is tedious work and easy to forget.
Chetter takes care of all that by creating references in
`refs/chetter/<pull request>/`.

Each version of a pull request, defined as push to a branch with an open pull
request, is tracked by `refs/chetter/<pull request>/v<version number>`.
Additionally, `refs/chetter/<pull request>/head` tracks the most recent version.

Similarly, a new reference is created each time a reviewer completes their
review (submits a review with either *Approval* or *Request changes*).  Each
review is tracked as `refs/chetter/<pull request>/<reviewer>-v<review number>`,
and `refs/chetter/<pull request>/<reviewer>-head` points to the most recent
review.

Finally, all of the references mentioned in the two prior paragraphs also have
an associated reference ending in `-base` which represents the base of the pull
request at the time the versioned reference was made.

When a pull request is closed or merged, Chetter will delete all associated
references.

## Pulling Chetter References
Enable automatic fetching of Chetter references:

    git config --add remote.origin.fetch '+refs/chetter/*:refs/chetter/origin/*'

You may wish to enable automatic pruning so that local references are deleted
when the pull request is closed.

    git config --add remote.origin.prune true

## Using Chetter References
What changed since you last reviewed pull request 10:

    git diff chetter/origin/10/<username>-head..chetter/origin/10/head

Commits and changes between v1 and v2 of pull request 10 with the same
base, *origin/master*:

    git range-diff origin/master chetter/origin/10/v1 chetter/origin/10/v2

Commits and changes since you last reviewed after the pull request branch was
rebased on new changes to *origin/master* and force pushed:

    git range-diff \
            chetter/origin/10/<username>-base..chetter/origin/10/<username>-head \
            chetter/origin/10/head-base..chetter/origin/10/head

# Running Chetter
- [Register a GitHub App](
    https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/registering-a-github-app)
    - Select the *Contents (read/write)* and *Pull Request (read-only)* Repository Permissions
    - Enable the *Pull Request* and *Pull Request Review* event subscriptions
    - Set the Webhook URL to point to where chetter-app will be running
    - Note the application id
    - Generate a private key

- Create a `chetter-app.toml` configuration

    ```
    app_id = <application_id>
    private_key = """
    -----BEGIN RSA PRIVATE KEY-----
    ABCDE...
    -----END RSA PRIVATE KEY-----
    """
    ```

- Build the chetter-app container image

    ```
    make -C docker VERSION=latest chetter-app
    ```

- Create a container
    - Expose port 3333/tcp
    - Add the directory containing `config.toml` as a volume mounted at `/config`

    ```
    podman run --rm \
        --name chetter-app
        --publish 0.0.0.0:3333:3333/tcp \
        --volume /dir/containing/config:/config \
        chetter-app:latest
    ```
