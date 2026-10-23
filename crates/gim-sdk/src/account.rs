//! Account deletion: the user's data on the app server, their chats and
//! their records on every hub, all authorized by the root key.

use gim_base::model::Membership;
use gim_base::StateStore;

use crate::transport::TransportError;
use crate::{Client, ClientError, Result};

impl<S: StateStore + 'static> Client<S> {
    /// Deletes the user's account for good, in this order:
    ///
    /// 1. the app server's data of the user (the encrypted settings
    ///    backup), if an app server is configured;
    /// 2. the user leaves every chat (best effort: the members see them
    ///    leave);
    /// 3. the home hub deletes the user, their devices, handles, recovery
    ///    backup and inbox (`DELETE /v2/users/{user_id}`, root-signed);
    /// 4. the other listed hubs do the same, best effort.
    ///
    /// A failure in step 1 or 3 fails the call; calling it again carries
    /// on. Returns the ids of the other hubs that could not be reached:
    /// their records expire on their own. Local data stays: sign out
    /// afterwards to delete it (gim-ffi `delete_account` does both).
    /// Needs the root key (every device of a user has it).
    pub async fn delete_account(&self) -> Result<Vec<String>> {
        self.require_session()?;
        if self.is_extension() {
            return Err(ClientError::InvalidInput(
                "an app extension cannot delete the account".into(),
            ));
        }
        let root = self.root_key()?;
        if let Some(app_server) = self.inner.app_server.as_deref() {
            app_server.delete_user_data(&root).await?;
        }
        let chats = self.read(|store, _| Ok(store.chats()?)).await?;
        for chat in chats.iter().filter(|c| c.membership != Membership::Left) {
            if let Err(err) = self.leave(&chat.group_id).await {
                tracing::warn!(group_id = %chat.group_id, error = %err, "cannot leave the chat; deleting the account anyway");
            }
        }
        match self.inner.transport.delete_user(&root).await {
            Ok(()) | Err(TransportError::NotFound(_)) => {}
            Err(err) => return Err(err.into()),
        }
        let home = self.home_hub();
        // Listed hubs without a verified transport in this run count as
        // unreached.
        let mut unreached: Vec<String> = self
            .read(|store, _| Ok(store.hubs()?))
            .await?
            .into_iter()
            .map(|h| h.hub_id)
            .filter(|id| home.as_ref() != Some(id))
            .collect();
        for (hub_id, _, transport) in self.verified_hubs() {
            unreached.retain(|id| *id != hub_id);
            match transport.delete_user(&root).await {
                Ok(()) | Err(TransportError::NotFound(_)) => {}
                Err(err) => {
                    tracing::warn!(%hub_id, error = %err, "cannot delete the account on this hub");
                    unreached.push(hub_id);
                }
            }
        }
        Ok(unreached)
    }
}
