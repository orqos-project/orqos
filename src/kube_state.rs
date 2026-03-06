use kube::Client as KubeClient;

pub struct KubeState {
    pub(crate) client: KubeClient,
    pub(crate) namespace: String,
}
