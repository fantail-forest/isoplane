public static class Application
{
    public static string Value => Dependency.Value + Shared.Value;
}
