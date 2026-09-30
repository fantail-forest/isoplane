<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
  <xsl:output method="xml" indent="yes" />
  <xsl:param name="Root" />
  <xsl:template match="/Project">
    <Project>
      <Import Project="Sdk.props" Sdk="Microsoft.NET.Sdk" />
      <Import Project="{$Root}/guest-sdks/dotnet/Build/Isoplane.ManagedBundle.props" />
      <PropertyGroup>
        <TargetFramework>net10.0</TargetFramework>
        <IsoplaneRuntimeDirectory>$(ISOPLANE_RUNTIME_DIRECTORY)</IsoplaneRuntimeDirectory>
      </PropertyGroup>
      <xsl:copy-of select="*" />
      <ItemGroup>
        <ProjectReference Include="{$Root}/guest-sdks/dotnet/Isoplane.Sdk.csproj"
                          GlobalPropertiesToRemove="RuntimeIdentifier;SelfContained"
                          AdditionalProperties="ArtifactsPath=$(ISOPLANE_MANAGED_ARTIFACTS);IsoplaneManagedOnly=true;UseAppHost=false" />
        <xsl:if test="contains(@Sdk, 'AspNetCore')">
          <ProjectReference Include="{$Root}/guest-sdks/dotnet/AspNetCore/Isoplane.AspNetCore.csproj"
                            GlobalPropertiesToRemove="RuntimeIdentifier;SelfContained"
                            AdditionalProperties="ArtifactsPath=$(ISOPLANE_MANAGED_ARTIFACTS);IsoplaneManagedOnly=true;UseAppHost=false" />
        </xsl:if>
      </ItemGroup>
      <Import Project="Sdk.targets" Sdk="Microsoft.NET.Sdk" />
      <xsl:if test="contains(@Sdk, 'AspNetCore')">
        <Import Project="{$Root}/guest-sdks/dotnet/AspNetCore/buildTransitive/Isoplane.AspNetCore.Framework.targets" />
      </xsl:if>
      <Import Project="{$Root}/guest-sdks/dotnet/Build/Isoplane.ManagedBundle.targets" />
      <Import Project="{$Root}/guest-sdks/dotnet/Build/Isoplane.Run.targets" />
    </Project>
  </xsl:template>
</xsl:stylesheet>
